# kirby-csms

`kirby-csms` is a terminal OCPP 1.6J central system (CSMS) for charge point firmware development and load tests. It accepts any number of stations, answers their CALLs with canned replies, prints everything that happens and sends commands to stations. On an interactive terminal it opens a terminal UI; otherwise it runs headless.

Contents: [Quick start](#quick-start) · [Connecting stations](#connecting-stations) · [Output](#output) · [Terminal UI](#terminal-ui) · [Commands on stdin](#commands-on-stdin) · [HTTP API](#http-api) · [Response rules and fault injection](#response-rules-and-fault-injection) · [Headless behavior](#headless-behavior) · [Exit codes](#exit-codes) · [Use as a library](#use-as-a-library) · [Related pages](#related-pages)

## Quick start

```sh
kirby-csms --listen 0.0.0.0:9000                          # TUI on a terminal, headless otherwise
kirby-csms --listen 127.0.0.1:0 --json --subprotocol omit # free port, JSON lines
kirby-csms --tls-cert cert.pem --tls-key key.pem          # wss://
kirby-csms --script suite.toml --junit junit.xml --timeout 120
echo '{"station":"CP-1","action":"Reset","payload":{"type":"Soft"}}' | kirby-csms
```

Point a station at it, for example the emulator: `kirby-ocpp --url ws://localhost:9000/ocpp --id CP-1`. Every flag and its `KIRBY_CSMS_*` variable is in [configuration.md](configuration.md); precedence is command line, then environment, then default. A TOML file keeps a whole setup (flags, `[[station]]` keys and `[[rule]]` response rules and faults) in one place: `--config FILE`, `--profile NAME`, `--print-config`, `--check-config`, see [kirby-csms configuration file](configuration.md#kirby-csms-configuration-file).

## Connecting stations

- URL: `ws(s)://HOST:PORT/<any prefix>/<station id>`. The station id is the last path segment, 1 to 128 bytes after percent-decoding, without control characters, line separators or bidi overrides (and not `.` or `..`). A connection whose URL holds no such id is refused with HTTP 400 and reported as an `auth_failed` event with `station` `null` and the reason `refused: no valid station id in the URL path (1 to 128 bytes, no control characters)`.
- A second connection with the same id replaces the first one (the old one shows as "replaced"). At security profile 0 nothing authenticates the new connection, so anybody who can reach the listener can take over a live station's id; use a profile or a firewall (see [security.md](security.md)).
- Default WebSocket subprotocol policy is `require:ocpp1.6` (`--subprotocol`: `require:NAME`, `select:NAME`, `omit`, `reject`).
- TLS and authentication: [security.md](security.md).
- Inbound CALLs are validated against the OCPP 1.6J schemas by default (`--validate-inbound`). An invalid payload gets a CALLERROR (`OccurenceConstraintViolation`, `TypeConstraintViolation`, `PropertyConstraintViolation` or `FormationViolation`) and is not tracked. A fault on the same action wins; unknown actions are not checked.
- Actions a charge point may not send get a CALLERROR: `NotSupported` for known action names, `NotImplemented` for unknown ones.
- Per station the server keeps the last `--history-cap` frames (default 1000, also capped at 8 MiB of text; a frame above that cap is kept as the marker `{"truncated":true,"originalBytes":N}`) and the last `--transaction-history-cap` stopped transactions (default 100). Frame events (`--json`, the HTTP event stream, `CsmsEvent::Frame`) carry the same marker for a frame above 64 KiB, so a stalled event consumer cannot hold large frames. Scripts (`wait_call` with `payload_contains` or `capture`) still see the full payload of such a frame: it is read from the station's history. If the history no longer holds it, the step fails with `payload truncated (N bytes)` instead of waiting for the timeout. A malformed text that mentions `AuthorizationKey` is shown as `<redacted: mentions AuthorizationKey>`. Records are never evicted automatically; `F` in the TUI (or `CsmsHandle::forget`) removes a disconnected station.

## Output

The first output line is the listen address, so `--listen 127.0.0.1:0` works in tests:

```text
listening on ws://127.0.0.1:41234
{"type":"listening","addr":"127.0.0.1:41234","url":"ws://127.0.0.1:41234"}   (with --json)
```

Under the TUI the `listening on ...` line goes to stderr before the screen opens.

After it come up to two more lines. With security profile 0 on a non-loopback listen address: `warning: security profile 0 on a non-loopback address: any peer can connect and take over a station id; see docs/security.md` (with `--json`: `{"type":"warning","text":"security profile 0 on a non-loopback address: ..."}`). Without `--json`: `note: stations connect to ws://<host>:<port>/<prefix>/<station id>`, where an unspecified listen host (`0.0.0.0`, `::`) is shown as `<this-host>`.

In headless runs (`--no-tui`, `--script`, a pipe) all of these lines go to stdout, through the same ordered queue as the events, so each line is whole and `listening on ...` is always first, also when stderr is redirected into the same file (`kirby-csms ... > csms.log 2>&1`). Under the TUI they go to stderr, before the screen opens. Errors always go to stderr.

Then one line per event:

| Event (`"type"`) | Text line | Notes |
| --- | --- | --- |
| `connected` | `HH:MM:SS.mmm CP-1 connected from 10.0.0.5:4242 (ocpp1.6, profile 2)` | JSON adds `remote_addr` and `protocol` (the negotiated WebSocket subprotocol, `null` when none was selected); the text shows `no subprotocol` then, and the security profile number |
| `disconnected` | `... CP-1 disconnected (reason)` | `reason` field in JSON, for example `ping_timeout` |
| `frame` | `... CP-1 <- [2,"id","Heartbeat",{}]` | `<-` from the station, `->` to it; JSON has `direction` `in` / `out` |
| `state` | `... CP-1 state: connectors [1=Available] active transactions [123]` | only when connector statuses or active transactions changed |
| `malformed` | `... CP-1 sent malformed text: ...` | |
| `forgotten` | `... CP-1 forgotten` | |
| `auth_failed` | `... CP-1 refused (reason) from ADDR` | JSON: `{"type":"auth_failed","station":..,"remote_addr":..,"reason":..}`; `station` is `null` when the TLS handshake failed or the URL holds no valid station id |
| `listening`, `api_listening` | `listening on URL`, `api listening on URL` | startup lines, see above; JSON has `addr` and `url` |
| `warning` | `warning: ...` | the security profile 0 warning at startup; JSON `{"type":"warning","text":..}` |
| `note` | `note: ...` | for example lines dropped because the output could not keep up, or the `stations connect to ...` startup line; JSON `{"type":"note","text":..}` |

JSON events carry `ts` (RFC 3339, milliseconds), except the startup and housekeeping events `listening`, `api_listening`, `warning` and `note`, which have none. Text supplied by stations is shown with control characters replaced, because station ids and payloads are attacker-controlled. `AuthorizationKey` values in frames are replaced by `***` unless `--show-secrets` is set; see [security.md](security.md#redaction-and---show-secrets).

`--log-file PATH` appends every event as a JSON line, also under the TUI. Stdout and the log file are written by helper threads: if nobody reads stdout, the process keeps running, the log file keeps being written, and the lines that did not fit are dropped with a `note` line saying how many.

## Terminal UI

With a terminal on both stdin and stdout and none of `--no-tui`, `--json`, `--script`, `--stdin`, the UI opens. `--no-tui` (or `KIRBY_CSMS_NO_TUI=1`) keeps it headless. If the terminal cannot be set up, the server runs headless and says so. Quitting the UI (`q`, `Ctrl-C`) shuts the server down at once, without a confirmation.

Layout:

- Left: the stations (dot: green connected, dim disconnected, yellow replaced by a newer connection; vendor and model; `txN` active transactions). The header shows the listen address, connected/total stations and frame counters.
- Right, top: details of the selected station (boot info, remote address, subprotocol, connectors, transactions with energy and power, last heartbeat, firmware and diagnostics status, counters).
- Right, bottom: the frame log of that station with CALL/RESULT correlation and round-trip times.
- Before any station has connected the details pane shows the listen address, a station URL to copy (`ws://<this-host>:9000/<station id>` for an unspecified listen host; a path prefix before the id is accepted) and, at security profile 0, a dim note that any peer can connect. The footer always ends with the `? help` and `: actions` hints, even in a narrow terminal, and the help popup opens with a short "First steps" paragraph.
- The UI needs at least 24 columns by 8 rows; a smaller terminal shows `need 24x8, have WxH` until it is resized. The help popup wraps to the terminal width, and long station ids are shortened in the middle so ids with a common prefix stay apart.
- Header chips: `security profile N`, `N auth failures` (refused connections also show as a status-line notice, at most one per second), `N pending` commands, and the pending fault count and rule actions while any is active.

Navigation:

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

Actions on the selected station:

| Key | Action |
| --- | --- |
| `t` | RemoteStartTransaction (idTag, optional connector) |
| `x` | RemoteStopTransaction (pick one of the active transactions) |
| `R` | Reset, soft or hard (asks first) |
| `c` `g` | ChangeConfiguration / GetConfiguration (keys optional) |
| `T` | TriggerMessage (message, optional connector) |
| `u` `A` `C` | UnlockConnector / ChangeAvailability / ClearCache |
| `P` | SetChargingProfile (connector and profile JSON, validated against the schema) |
| `K` `H` | ClearChargingProfile (asks first when no filter is set, because every profile is cleared) / GetCompositeSchedule (the answer opens in a popup) |
| `V` | ReserveNow |
| `U` `O` | UpdateFirmware / GetDiagnostics |
| `B` | SendLocalList (Differential or Full; entries as comma separated `idTag[:status]`, status defaults to Accepted, `idTag:-` deletes in a differential update, at most 100 entries) |
| `D` | DataTransfer |
| `w` | raw call: any central system action with a JSON payload checked against its request schema |
| `l` | GetLog |
| `I` `d` `L` | InstallCertificate (paste the PEM; line breaks become spaces in the field and are rebuilt on submit) / DeleteCertificate / GetInstalledCertificateIds |
| `E` | ExtendedTriggerMessage (`SignChargePointCertificate` preselected) |
| `S` `X` | SignedUpdateFirmware / CertificateSigned |
| `e` | the last 100 security events the station reported (scrollable); the Station pane shows the last three, the last log and signed firmware status and a CSR marker |
| `r` | the last results of the selected station (`Enter` opens one) |
| `m` | response rule (all stations), see below |
| `i` | inject a fault (all stations), see below |
| `M` | clear every response rule and pending fault (asks first); one rule can be removed from the palette |
| `F` | forget a disconnected station and its state (asks first) |
| `n` | copy the station id |

Forms: `Tab` next field, `Left` `Right` or a letter picks an option, `Enter` sends, `Esc` cancels. Fields are checked as you type and the request is checked against the OCPP 1.6J schema before anything is sent; errors show under the field. A field takes at most 256 KiB (typed or pasted; the rest is dropped and the field says so). DataTransfer limits `vendorId` to 255 characters, `messageId` to 50 and `data` to 64 KiB. Ids and numbers are trimmed; the values of ChangeConfiguration and DataTransfer `data` keep their leading and trailing spaces.

Results: commands run in the background, several can be in flight (the header shows `N pending`). The answer appears in the status line (`Reset (Soft) on CP-1: Accepted`, or the CALLERROR code and text, or a timeout) and is kept in a history of the last 20 results per station (`r`). Answers that carry content (GetConfiguration as a readable key list with `(readonly)` marks and unknown keys, raw calls, DataTransfer with data, GetCompositeSchedule, hash data) open in a popup (`c` copies).

## Commands on stdin

Without `--script`, when stdin is not a terminal (or with `--stdin`), each input line is a JSON object and gets exactly one answer line. End of input does not stop the server. Answers are always JSON, also without `--json`.

```text
{"station":"CP-1","action":"ChangeConfiguration","payload":{"key":"HeartbeatInterval","value":"60"},"timeout_ms":5000,"id":"my-tag"}
-> {"type":"result","station":"CP-1","action":"ChangeConfiguration","ok":true,"result":{"status":"Accepted"},"id":"my-tag"}
-> {"type":"result","station":"CP-1","action":"Reset","ok":false,"error":"CALLERROR NotSupported: ..."}
{"cmd":"stations"}
-> {"type":"stations","stations":[{"active_transactions":[],"connected":true,"connectors":{"1":"Available"},"id":"CP-1","model":"...","protocol":"ocpp1.6","received_calls":3,"remote_addr":"127.0.0.1:4242","security_events":0,"vendor":"..."}]}
```

| Field | Meaning |
| --- | --- |
| `station`, `action` | required for a CALL; strings |
| `payload` | request payload, default `{}`; sent as given, not schema-checked, so malformed requests can be tested |
| `timeout_ms` | 1 to 86 400 000, default `--call-timeout` (30 s) |
| `id` | optional tag echoed in the answer |
| `cmd` | `"stations"` instead of a CALL; the only command |

Rules:

- Answers can arrive out of order (a slow command does not hold back later ones). Match them by `id`, not by position.
- A CALLERROR, a timeout, an unknown station or a bad line (invalid JSON, not an object, invalid UTF-8, longer than 1 MiB) is an `"ok":false` result with an `error` text, never a crash. A CALLERROR answer also carries `code` and `details`.
- At most 256 commands are in flight; the reader waits while all slots are busy.
- Response rules and faults are not available over stdin; use the TUI or a [script](scenarios.md).

## HTTP API

`--api-listen 127.0.0.1:9100` starts a local JSON HTTP API next to the server (off by default; works with the TUI, headless and `--script`): list stations, read state and frame history, send CALLs, forget stations, stream events (SSE) and manage response rules and faults.
- Calls share the 256-command limit with stdin. The answer is the stdin result object without `type`, `station` and `action`, plus an `outcome` field; stdin answers are unchanged. After the `listening` line the server prints `api listening on http://ADDR` (`{"type":"api_listening","addr":..,"url":..}` with `--json`).
- A non-loopback address needs `--api-token` / `--api-token-file`; without a token browser-like requests (`Origin`, cross-site, non-loopback `Host`) are refused. Plain HTTP only. The shapes are a versioned contract (`/v1`, `api_version` 1; a release that serves OCPP 2.0.1 or 2.1 on the same listener keeps them). Endpoints, error codes, auth model and examples: [api.md](api.md).

## Response rules and fault injection

Both apply to all stations, not to the selected one, and to CALLs sent by stations (not to commands sent by the CSMS).

- Response rule (`m`): answer every CALL of one action with a fixed CALLRESULT payload, for example `{"status":"Rejected"}` for `Authorize`. The payload is not validated, so it can break the schema on purpose. Remove one rule from the palette; `M` clears all.
- Fault (`i`): for the next N occurrences of an action (N up to 1 000 000) do something else than the normal answer. Faults win over rules and over the canned replies.

| Fault kind | Effect |
| --- | --- |
| `call_error` | CALLERROR `InternalError` ("injected CALLERROR") |
| `silence` | no reply; the station's call times out |
| `close` | close the WebSocket with code 1011 without replying |
| `invalid_payload` | CALLRESULT with the payload you give, as is (for example schema-invalid) |

Scripts and the HTTP API check faults the same way: the action name is 1 to 128 bytes without control characters, `times` is 1 to 1 000 000, and at most 1000 faults (8 MiB of text) are stored at once.

Rules and faults can also be installed at start from `[[rule]]` blocks of a [configuration file](configuration.md#blocks) (`behavior = "respond"` or a fault kind). They apply to all stations and are in place before the first station connects.

In scripts the same exist as the `respond`, `clear_respond` and `inject_fault` steps, see [scenarios.md](scenarios.md). The emulator has the mirror feature for CSMS commands ([emulator.md](emulator.md)).

## Headless behavior

- Events go to stdout (text or `--json`); `--log-file` also gets them.
- Stdin commands are read when stdin is not a terminal and there is no `--script` (or always with `--stdin`).
- `--script FILE` runs a TOML script against the stations that connect, prints the report (`--json` for JSON), writes `--report` (JSON) and `--junit` (XML), then exits. `--timeout SECONDS` (default 60) bounds the run. Script syntax (steps, `[vars]`, `--var`, `capture`, `expect_payload`): [scenarios.md](scenarios.md#csms-scripts).
- SIGINT or SIGTERM shuts the server down gracefully. During `--script`, SIGTERM still writes the `--junit` file (one failed suite, `interrupted`) and exits 1; SIGINT exits 130 without a report.
- `--ping-interval` / `--pong-timeout`: a WebSocket ping goes to each station after this much silence; a station that sends nothing for the timeout after a ping is dropped (disconnect reason `ping_timeout`). A non-zero ping interval below the pong timeout is a usage error.

## Exit codes

`kirby-csms` exits 0 on success or a clean shutdown, 1 when a script fails or times out or the server cannot start, 2 on usage, configuration or script errors, and 130 when a script is interrupted by SIGINT. The full table is in [configuration.md](configuration.md#exit-codes).

## Use as a library

`kirby-csms-core` is the UI-free engine (published as a crate). It exposes `CsmsServer::start(CsmsConfig) -> CsmsHandle`; everything else goes through the handle:

| Item | Purpose |
| --- | --- |
| `CsmsHandle::call` / `call_timeout` / `call_typed` and command helpers (`reset`, `remote_start_transaction`, ...) | send a CALL to a station and await the answer; typed ones are schema-checked |
| `subscribe()` | `broadcast::Receiver<CsmsEvent>` (`Connected`, `Disconnected`, `Frame`, `StateChanged`, `Malformed`, `Forgotten`, `AuthFailed`) |
| `stations()`, `station(id)`, `history(id)`, `state(id)` | read model (`StationInfo`, `FrameEntry`, `StationState`) |
| `respond(action, closure)`, `clear_responder`, `inject(Fault)`, `clear_faults`, `faults_remaining` | response rules and faults |
| `forget(id)`, `shutdown()`, `url()`, `local_addr()` | housekeeping |

```rust
use kirby_csms_core::{CsmsConfig, CsmsError, CsmsEvent, CsmsServer};
use serde_json::json;

async fn run() -> Result<(), CsmsError> {
    let csms = CsmsServer::start(CsmsConfig::new("0.0.0.0:9000".parse().unwrap())).await?;
    let mut events = csms.subscribe();
    while let Ok(event) = events.recv().await {
        if let CsmsEvent::Connected { station } = event {
            let reply = csms
                .call(&station, "ChangeConfiguration", json!({ "key": "HeartbeatInterval", "value": "60" }))
                .await?;
            println!("{station}: {}", reply.to_json_string());
        }
    }
    csms.shutdown().await;
    Ok(())
}
```

`CsmsConfig` fields mirror the CLI flags (listen address, subprotocol and transaction id policies, heartbeat interval, history caps, `tls`, `security`, `show_secrets`, `validate_inbound`, ping settings). A subscriber that falls behind the event channel (4096 events) gets `RecvError::Lagged` and loses events. Internals: [architecture.md](architecture.md#csms-internals).

## Related pages

- [configuration.md](configuration.md): every flag and environment variable
- [api.md](api.md): the local HTTP control API
- [security.md](security.md): profiles, TLS, certificates
- [scenarios.md](scenarios.md): TOML scripts and scenarios
- [emulator.md](emulator.md): the charge point side
- [architecture.md](architecture.md): crate map and contracts
