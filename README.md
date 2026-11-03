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

Or download a prebuilt binary for Linux (x86_64 glibc, aarch64 glibc, x86_64 static musl for Alpine and minimal containers), macOS or Windows from the [GitHub releases](https://github.com/archhaze24/kirby-ocpp/releases). To build from a checkout: `cargo install --path crates/kirby-ocpp`.

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
| `--count` | `OCPP_COUNT` | `1` | Number of stations (fleet mode, 1-1000), see [Fleet mode](#fleet-mode) |
| `--id-template` | `OCPP_ID_TEMPLATE` | the `--id` value | Charge point id template with `{n}` / `{n:03}` |
| `--first-index` | `OCPP_FIRST_INDEX` | `1` | Index of the first station |
| `--stagger-ms` | `OCPP_STAGGER_MS` | `0` | Delay between starting two stations (ms) |
| `--rule ACTION=BEHAVIOR[*N]` | `KIRBY_OCPP_RULES` (`;` separates rules) | none | Response rule for a CSMS command, repeatable; see [Response rules](#response-rules) |
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

Per charge point id, one JSON file `<state dir>/<id>.station.json` holds the local authorization list and its version, authorization cache, charging profiles, writable configuration values, availability (including a `Scheduled` change waiting for a transaction to end), connector count and per-connector state (status, meter, reservation, transaction) and the offline transaction queue. Ids whose escaped name would exceed 100 bytes are shortened and get a hash suffix (`...~<16 hex>.station.json`) so the name always fits the filesystem limit. Files are versioned and written atomically (temp file plus rename, then the directory is fsynced); a corrupt file or one belonging to another charge point is moved to `<file>.bak` and the station starts clean. `--no-persist` disables both loading and saving.

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

Actions: connect/disconnect/reconnect, BootNotification, Heartbeat, add connector, response rules (`m` adds one, `Action=off` removes one, `M` clears all; active rules are listed in the Station pane); plug/unplug, set status, StatusNotification, fault toggle; Authorize, StartTransaction, StopTransaction (with reason), MeterValues; set diagnostics/firmware outcome and send their status notifications; DataTransfer, vendor ping and raw CALLs. Security Whitepaper (Station tab): `C` requests a certificate (key pair, CSR, `SignCertificate`), `S` reports a security event (pick one of the 18 types, optional techInfo), `Y` opens the certificate browser; the Station pane shows the security profile, certificate count, queued security events, a pending CSR and the last log-upload and signed-firmware status with their request ids.

Data browser (Station tab: `K` config keys, `L` local authorization list, `P` charging profiles, `Y` certificates (read-only: type, subject, expiry; install/delete come from the CSMS); also in the palette): one popup with four pages, switched with `Tab`/`Shift-Tab`, `Left`/`Right` or `1`-`4`. `/` filters the rows fuzzily (`Enter` keeps the filter, `Esc` clears it), `Up/Down`/`j/k`/`PageUp/PageDown`/`g`/`G` move, `Esc`/`q` closes. Forms and confirmations opened from the browser return to it. Edits are local commands that behave like the CSMS-triggered ones, without a CSMS round trip:

| Page | Keys |
| --- | --- |
| Config keys | `Enter`/`e` edits the selected key (read-only keys are marked `(ro)` and refuse); the value is checked live with the station's own rules, as `ChangeConfiguration` would, and applied with the same side effects (heartbeat/metering timers) and persistence |
| Local auth list | `a` adds, `Enter`/`e` edits (idTag, status, expiry, parent), `d` removes, `c` clears (asks first). Each add/edit/remove is a differential update that bumps the list version by one; clear empties the list and resets the version to 0 |
| Charging profiles | `i` installs a profile from JSON (a template is prefilled; paste your own: `csChargingProfiles` object plus connector, validated against the `SetChargingProfile` schema and the station's profile rules), `e` edits the selected one (same id replaces it), `Enter` shows its JSON (`c` there copies), `d` deletes it, `c` clears all (asks first) |

The snapshot behind it (`Snapshot::config`, `local_list`, `charging_profiles`) is also available to API users; the local list view holds at most 500 entries (`total` has the real count).

Log view (Logs tab): CALL/RESULT/ERROR correlation, filters (all, calls, errors, station, csms; `f`), search (`s`), pause/follow (`p`), jump to newest (`G`), detail view with scroll (`Enter`; `c`/`y` copies), copy row (`y`) or visible rows (`Y`), export as JSONL (`e`, default `./kirby-ocpp-<id>-<timestamp>.jsonl`), clear with confirmation (`c`). The buffer keeps the last 2000 entries. Copying tries external clipboard tools first and falls back to the terminal's OSC 52 sequence.

Scenarios tab: one `Run: <name>` action per built-in scenario (keys `o`, `w`, `s`, `i` for the first four) and `l` to load a scenario file, `x` to cancel the running one.

## Headless mode and exit codes

Without a TTY (or with `--no-tui`) the tool connects and prints events until SIGINT/SIGTERM, then closes the WebSocket cleanly. With `--json` each line is one JSON object; frames carry a `payload` field.

`kirby-ocpp run --scenario <name|file.toml> [--report report.json] [--junit junit.xml] [--timeout 60] [--id-tag TAG]` connects, waits for an accepted BootNotification, runs the scenario and prints a PASS/FAIL/SKIP report (a JSON object with `"type": "report"` under `--json`). `--junit junit.xml` also writes a JUnit XML report for CI: one `<testsuite>` per run, one `<testcase>` per step (failed step = `<failure>`, skipped = `<skipped/>`); a run that could not connect or boot is a suite with one errored `setup` case.

| Code | Meaning |
| --- | --- |
| 0 | Success (scenario passed, normal shutdown) |
| 1 | Scenario failed or timed out, or a runtime error |
| 2 | Usage or configuration error |
| 130 | `run` interrupted by SIGINT (Ctrl-C); SIGTERM still ends it with 1 |

## Fleet mode

`--count N` runs N independent stations in one process against the same CSMS, for load and multi-station tests (terminal UI on a TTY, headless with `--no-tui` or without one).

```
kirby-ocpp --url ws://localhost:9000/ocpp --id 'CP-{n:03}' --count 50 --stagger-ms 100 --no-tui
kirby-ocpp --url ws://localhost:9000/ocpp --id 'CP-{n}' --count 20 run --scenario charge-session
```

- The id comes from `--id-template`, or from `--id` when no template is given. `{n}` is the station index, `{n:03}` the index zero-padded to 3 digits; `{{` and `}}` are literal braces. With `--count` above 1 the template needs a placeholder (otherwise exit 2). `--first-index` (default 1) is the index of the first station, so `--count 3 --first-index 10` gives `CP-10`, `CP-11`, `CP-12`. `--count 1` without `--id-template` uses `--id` as is; with an explicit template it is expanded too.
- Every station is a normal station (own connection, transactions, state file named after its id, so state files never collide). `--stagger-ms` waits that long between starting two stations.
- The terminal UI shows a selector row on top (`1 CP-001+* 2 CP-002- ...`: index, id, connection glyph `+` connected, `~` connecting/retrying, `-` offline, `*` a transaction is running) and the selected station below. Everything else (status, log, actions, `K`/`L`/`P` browsers, forms, scenarios) works on the selected station only; each station keeps its own log (last 500 rows), dialogs and scenario progress, and events of the other stations keep being recorded. `Alt` combinations may need "Option as Meta" in macOS terminals. With `--count 1` there is no selector and no extra keys. Quit stops the whole fleet.
- Output is tagged with the station id: `[CP-001] ...` in front of every human line, a `"station"` field in every JSON line, stdout and `--log-file` alike. With `--count 1` nothing is tagged.
- `run --scenario` runs the scenario on every station at the same time and prints one report per station (tagged), then one aggregate line: `fleet: 3/3 stations PASSED`, or `fleet: 2/3 stations passed, FAILED: CP-2`. With `--json` the aggregate is `{"type":"fleet_report","passed":bool,"total":n,"passed_count":n,"failed":[ids],"stations":[...]}`. `--timeout` covers the whole fleet, stagger included. `--report out.json` writes one file per station: `out.CP-001.json` (characters of the id other than letters, digits, `-` and `_` become `_`). `--junit` writes one file for the whole fleet, with a suite `scenario [CP-001]` per station.

| Fleet key | Action |
|---|---|
| `<` `>` | Previous / next station |
| `Alt-1` .. `Alt-9` | Jump to station 1..9 |
| `Alt-c` / `Alt-d` | Connect / disconnect all stations |

Selector keys work in normal mode only (inside a dialog they are plain input).
- Exit codes: 0 only if every station passes, 1 if any fails or times out, 2 usage error, 130 SIGINT.
- `--print-config` with `--count > 1` prints `{"fleet": {...}, "config": {...}}` where `config` is the first station.

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

Actions: `connect`, `disconnect`, `reconnect`, `boot`, `heartbeat`, `plug`, `unplug`, `authorize`, `start_tx`, `stop_tx`, `meter`, `status`, `fault`, `clear_fault`, `sleep`, `wait_for`, `expect_call` (`call`, optional `direction`, `match` payload subset), `send_raw` (`call`, `payload`, `validate`), `data_transfer`, `set_rule` (`rule`), `clear_rules` (optional `call`), `request_certificate` (Security Whitepaper: key pair + CSR + `SignCertificate`), `security_event` (`event_type`, optional `tech_info`; the event type is checked when the scenario loads). Every step may have `name` and `expect` (`status`, `payload` subset, `error = true`). `wait_for` takes `condition = { kind = ... }` with kinds `connected`, `disconnected`, `registration`, `connector_status`, `tx_active`, `tx_idle`, `no_pending_messages`, and `timeout_ms` (default 5000). Unknown fields are rejected.

## Response rules

Response rules make the station misbehave on purpose, to test how a CSMS reacts. A rule overrides how one CSMS command (any of the 23 the station handles: the 19 of OCPP 1.6 and the four certificate commands of the Security Whitepaper, e.g. `Reset`, `RemoteStartTransaction`, `InstallCertificate`) is answered. Form: `ACTION=BEHAVIOR[*COUNT]`, action names case-insensitive.

| Behavior | Effect |
| --- | --- |
| `reject` | The action's own rejection (`Rejected`; `NotSupported` for `SendLocalList` and `UnlockConnector`). `GetConfiguration`, `GetLocalListVersion`, `ClearChargingProfile`, `GetDiagnostics` and `UpdateFirmware` have no rejection status, so they get CALLERROR `NotSupported`. |
| `delay:MS` | The command is handled normally; only the reply is sent `MS` ms later (0 to 3600000). The station keeps working meanwhile. |
| `error:CODE[:TEXT]` | CALLERROR with an OCPP 1.6 code (`NotImplemented`, `NotSupported`, `InternalError`, `ProtocolError`, `SecurityError`, `FormationViolation`, `PropertyConstraintViolation`, `OccurenceConstraintViolation`, `TypeConstraintViolation`, `GenericError`). |
| `drop` | Close the WebSocket without replying; the normal reconnect lifecycle follows (disabled by `--no-reconnect`). |
| `ignore` | Never reply, so the CSMS call timeout fires. |

`*COUNT` applies the rule that many times and then removes it (default: unlimited). There is one rule per action; setting another replaces it. Except for `delay`, the command is **not executed** (no side effects). Rules are runtime only, never persisted. Set them with `--rule` (repeatable) or `KIRBY_OCPP_RULES`; in both, `;` separates several rules, so a rule (e.g. an `error:` text) must not contain `;`, in the TUI (Station tab, `m`), as scenario steps, or through `Command::SetResponseRule` / `ClearResponseRules` in the core API. Each application is logged (`response rule: Reset -> reject, 2 more`).

```sh
kirby-ocpp --url ws://localhost:9000/ocpp --rule Reset=reject --rule GetConfiguration=delay:5000 \
  --rule ChangeConfiguration=error:InternalError*3 --rule RemoteStartTransaction=ignore*1
```

In scenarios: `action = "set_rule"` with `rule = "Reset=reject*2"`, and `action = "clear_rules"` with an optional `call = "Reset"` (all rules when omitted).

## kirby-csms

`kirby-csms` is a terminal OCPP 1.6J central system (CSMS) for charge point firmware developers and load tests. It accepts any number of stations on `ws(s)://HOST:PORT/<any prefix>/<station id>` (the station id is the last path segment), answers their CALLs with canned replies, prints everything that happens and sends commands to stations. On an interactive terminal it opens a terminal UI to observe stations and send them commands; otherwise it runs headless (commands then come from stdin JSON lines or scripts).

```
kirby-csms --listen 0.0.0.0:9000
kirby-csms --listen 127.0.0.1:0 --json --subprotocol omit
kirby-csms --tls-cert cert.pem --tls-key key.pem
kirby-csms --script suite.toml --junit junit.xml --timeout 120
```

The first output line is `listening on ws://ADDR` (JSON: `{"type":"listening","addr":"127.0.0.1:41234","url":"ws://127.0.0.1:41234"}`), so `--listen 127.0.0.1:0` works for tests. Then one line per event: station connected / disconnected (with reason), every frame in and out, and a compact line when connector statuses or active transactions of a station change. SIGINT or SIGTERM shuts down gracefully.

| Flag | Env (`KIRBY_CSMS_`) | Default | Meaning |
| --- | --- | --- | --- |
| `--listen ADDR` | `LISTEN` | `0.0.0.0:9000` | address, port 0 picks a free one |
| `--tls-cert FILE`, `--tls-key FILE` | `TLS_CERT`, `TLS_KEY` | none | PEM, both or none, serves `wss://` |
| `--tls-client-ca FILE` | `TLS_CLIENT_CA` | none | CA certificate(s) (PEM) clients must be signed by (mutual TLS); needs `--tls-cert` |
| `--security-profile 0..3` | `SECURITY_PROFILE` | 0 | Security Whitepaper profile, see below |
| `--auth-key ID=KEY` | `AUTH_KEY` (`ID=KEY;ID=KEY`) | none | Basic Auth key of one station (repeatable) |
| `--auth-keys-file FILE` | `AUTH_KEYS_FILE` | none | lines `ID=KEY` (`#` comments); keep it private, its permissions are not checked |
| `--default-auth-key KEY` | `DEFAULT_AUTH_KEY` | none | key for stations without an entry |
| `--subprotocol POLICY` | `SUBPROTOCOL` | `require:ocpp1.6` | `require:NAME`, `select:NAME`, `omit`, `reject` |
| `--transaction-ids POLICY` | `TRANSACTION_IDS` | `fixed:123` | `fixed:ID` or `incrementing:FIRST` |
| `--heartbeat-interval SECONDS` | `HEARTBEAT_INTERVAL` | 300 | interval in the BootNotification answer |
| `--history-cap N` | `HISTORY_CAP` | 1000 | frames kept per station |
| `--transaction-history-cap N` | `TRANSACTION_HISTORY_CAP` | 100 | stopped transactions kept per station |
| `--call-timeout SECONDS` | `CALL_TIMEOUT` | 30 | wait for a station's answer to a command |
| `--json` | `JSON` | off | JSON lines instead of text |
| `--log-file PATH` | `LOG_FILE` | none | append every event as a JSON line |
| `--no-tui` | `NO_TUI` | off | never open the terminal UI, log to stdout |
| `--stdin` | `STDIN` | auto | read JSON-line commands even on a terminal |
| `--script FILE` | `SCRIPT` | none | run a TOML script, print the report, exit |
| `--report PATH`, `--junit PATH` | | none | script report as JSON / JUnit XML |
| `--timeout SECONDS` | | 60 | overall limit of a script run |
| `--licenses` | | | print third-party notices |

Command line beats environment beats default; booleans are strict (`true/false/yes/no/y/n/on/off/1/0`).

**Security profiles** (OCPP 1.6 Security Whitepaper): `0` accepts everybody. `1` requires HTTP Basic Auth (user name = station id from the URL, password = that station's key) over `ws://` or `wss://`; `2` is the same over TLS (`--tls-cert`/`--tls-key` required); `3` requires TLS with a client certificate signed by `--tls-client-ca` whose Common Name equals the station id. Profiles 1 and 2 need at least one key (`--auth-key`, `--auth-keys-file` or `--default-auth-key`); a station without a key is refused. The key is compared byte for byte with the password the station sends (the station's `AuthorizationKey` configuration value). Refusals answer HTTP 401 (403 for certificates) and print an `auth_failed` event (JSON: `{"type":"auth_failed","station":..,"remote_addr":..,"reason":..}`); keys are never printed. A failed TLS handshake under profile 3 is reported with `"station":null`. Invalid flag combinations exit with code 2. The CSMS also tracks what stations report (security events, `LogStatusNotification`, `SignedFirmwareStatusNotification`, the last `SignCertificate` CSR) in each station's state, and the engine has typed commands for `CertificateSigned`, `InstallCertificate`, `DeleteCertificate`, `GetInstalledCertificateIds`, `ExtendedTriggerMessage`, `GetLog` and `SignedUpdateFirmware` (headless: raw `action`/`payload` commands).

### Terminal UI

With a terminal on both stdin and stdout, and none of `--no-tui`, `--json`, `--script`, `--stdin`, `kirby-csms` opens a terminal UI (`--no-tui` or `KIRBY_CSMS_NO_TUI=1` keeps it headless). The `listening on ...` line goes to stderr before the screen opens; if the terminal cannot be set up, the server runs headless and says so. `--log-file` still receives every event. Quitting the UI (`q`, Ctrl-C) shuts the server down.

Left: the stations (dot: green connected, dim disconnected, yellow replaced by a newer connection; vendor model; `txN` active transactions), header with the listen address, connected/total stations and frame counters. Right: details of the selected station (boot info, remote address, subprotocol, connectors, transactions with energy and power, last heartbeat, firmware/diagnostics status, counters) above the frame log of that station, with CALL/RESULT correlation and round-trip times.

| Key | Action |
| --- | --- |
| `Up` `Down` `j` `k`, `PgUp` `PgDn`, `Home` `End` | move in the focused pane |
| `Left` `Right` `Tab` `Shift-Tab` | focus the station list / the log |
| `/` | filter stations by id (fuzzy); in the log pane: search the log |
| `Esc` | clear the log search, then the station filter |
| `a` | log of the selected station / merged live log of all stations |
| `Enter` (log) | frame detail with pretty JSON and its request or answer (`c` copies) |
| `y` `Y` | copy the row / all visible rows (clipboard tool, else OSC 52) |
| `f` `s` `p` `G` | log filter (all, calls, errors, from stations), search, pause/follow, newest |
| `:` | command palette: fuzzy search over every action below; unavailable ones are dimmed with the reason |
| `?` | help |
| `q`, `Ctrl-C` | quit |

Connections refused by the security profile show as a status-line notice (at most one per second) and a `N auth failures` chip in the header, next to a `security profile N` chip. Actions on the selected station open a form (`Tab` next field, `Left` `Right` or a letter picks an option, `Enter` sends, `Esc` cancels). Fields are checked as you type, and the request is checked against the OCPP 1.6J schema before anything is sent; errors show under the field. Commands run in the background (several can be in flight, the header shows `N pending`); the answer appears in the status line (`Reset (Soft) on CP-1: Accepted`, or the CALLERROR code and text, or a timeout) and is kept in a history of the last 20 results per station. Answers that carry content (GetConfiguration as a readable key list with `(readonly)` marks and unknown keys, raw calls, DataTransfer with data) open in a popup (`c` copies).

| Key | Action |
| --- | --- |
| `t` | RemoteStartTransaction (idTag, optional connector) |
| `x` | RemoteStopTransaction (pick one of the active transactions) |
| `R` | Reset, soft or hard (asks first) |
| `c` `g` | ChangeConfiguration / GetConfiguration (keys optional) |
| `T` | TriggerMessage (message, optional connector) |
| `u` `A` `C` | UnlockConnector / ChangeAvailability / ClearCache |
| `P` | SetChargingProfile (connector and profile JSON, validated against the schema) |
| `D` | DataTransfer |
| `l` | GetLog (log type, request id, upload URL, optional retries and time range) |
| `I` `d` | InstallCertificate (paste the PEM; line breaks become spaces in the field and are rebuilt on submit) / DeleteCertificate (hash algorithm and the three hashes) |
| `L` | GetInstalledCertificateIds (the hash data opens as a readable list) |
| `E` | ExtendedTriggerMessage (`SignChargePointCertificate` preselected) |
| `S` `X` | SignedUpdateFirmware (URL, retrieve time, request id, signing certificate PEM, signature) / CertificateSigned (PEM chain) |
| `e` | the last 100 security events the station reported (scrollable); the Station pane shows the last three, the last log and signed firmware status and a CSR marker |
| `w` | raw call: any of the 26 central system actions with a JSON payload checked against its request schema |
| `r` | the last results of the selected station (`Enter` opens one) |
| `m` | response rule: answer every station's CALLs of one action with a fixed payload |
| `i` | inject a fault for the next N occurrences of an action: `call_error`, `silence`, `close`, `invalid_payload` |
| `M` | clear every response rule and pending fault (asks first); one rule can be removed from the palette |
| `F` | forget a disconnected station and its state (asks first) |
| `n` | copy the station id |

Rules and faults apply to all stations, not only the selected one; while any is active the header shows the pending fault count and the rule actions.

### Commands on stdin

Without `--script`, when stdin is not a terminal (or with `--stdin`), each input line is a JSON object and gets exactly one answer line. End of input does not stop the server. Answers are always JSON, also without `--json`.

```
{"station":"CP-1","action":"ChangeConfiguration","payload":{"key":"HeartbeatInterval","value":"60"},"timeout_ms":5000,"id":"my-tag"}
-> {"type":"result","station":"CP-1","action":"ChangeConfiguration","ok":true,"result":{"status":"Accepted"},"id":"my-tag"}
-> {"type":"result","station":"CP-1","action":"Reset","ok":false,"error":"CALLERROR NotSupported: ..."}
{"cmd":"stations"}
-> {"type":"stations","stations":[{"id":"CP-1","connected":true,"vendor":"...","model":"...","connectors":{"1":"Available"},"active_transactions":[]}]}
```

`payload` defaults to `{}`, `timeout_ms` to `--call-timeout`, `id` is optional and echoed. Payloads are sent as given (not schema-checked), so malformed requests can be tested. A CALLERROR, a timeout, an unknown station or a bad line is an `"ok":false` result with an `error` text, never a crash.

### Scripts

A script is TOML with sequential steps; the first failing step stops the run (later steps are `SKIP`). The stations are external processes (for example `kirby-ocpp`) that connect while the script waits.

```toml
name = "smoke"

[[steps]]
action = "wait_station"          # station = "CP-1" and/or count = N; default timeout 30000 ms
station = "CP-1"
booted = true                    # also wait for its BootNotification (default: the socket is open)

[[steps]]
action = "wait_call"             # an inbound CALL; optional station and payload_contains (subset match)
call = "StartTransaction"
payload_contains = { connectorId = 1 }
timeout_ms = 20000

[[steps]]
action = "call"                  # raw CALL; expect_status checks payload.status, expect_error a CALLERROR
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "HeartbeatInterval", value = "60" }
expect_status = "Accepted"
```

`wait_call` consumes the CALL it matches: each inbound CALL satisfies at most one `wait_call`, but waits may come in any order (a later wait can match a CALL that arrived earlier than the previous wait's match). CALLs that arrive while other steps run are kept (the last 10 000).

Other steps: `sleep {ms}`, `respond {call, payload}` (answer every such CALL with this CALLRESULT payload), `clear_respond {call}`, `inject_fault {call, kind = "call_error" | "silence" | "close" | "invalid_payload", payload, times}`. Every step takes an optional `name`; `timeout_ms` defaults to 5000. The report looks like `kirby-ocpp run` (`PASS` / `FAIL` / `SKIP` per step; `--json` for a JSON report line).

Exit codes: 0 success or clean shutdown, 1 script failed or timed out (or the server could not start), 2 usage, configuration or script error, 130 script interrupted by SIGINT.

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
