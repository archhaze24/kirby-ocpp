# Scenarios and scripts

Two TOML formats drive a test run without a human:

* an emulator **scenario** drives one or more `kirby-ocpp` stations against a CSMS under test ([Emulator scenarios](#emulator-scenarios));
* a `kirby-csms` **script** drives the central system and waits for stations that connect ([CSMS scripts](#csms-scripts)).

Both stop at the first failing step, print a PASS/FAIL/SKIP report, can write a JSON report and a JUnit XML file, and use the same exit codes ([Reports](#reports)).

Contents: [Emulator scenarios](#emulator-scenarios) · [Running scenarios](#running-scenarios) · [Suite runs](#suite-runs) · [Fleet runs](#fleet-runs) · [CSMS scripts](#csms-scripts) · [Reports](#reports) · [Related pages](#related-pages)

## Emulator scenarios

A scenario file has a `name`, an optional `description` and a list of `[[steps]]`. Unknown fields anywhere are rejected when the file is loaded, with the step number in the message (`step 3: ...`). A scenario needs at least one step.

### Example

```toml
name = "session-with-rejected-reset"
description = "Reject a Reset, run a session, check what the CSMS answered"

[[steps]]
action = "set_rule"
rule = "Reset=reject*1"           # the next Reset from the CSMS is refused

[[steps]]
action = "plug"
connector = 1

[[steps]]
action = "start_tx"
connector = 1
id_tag = "KIRBY-TAG"              # implicitly expects status Accepted

[[steps]]
action = "wait_for"
condition = { kind = "tx_active", connector = 1 }
timeout_ms = 5000

[[steps]]
action = "meter"
connector = 1
wh_delta = 1000

[[steps]]
name = "meter values reached the CSMS"
action = "expect_call"
call = "MeterValues"
direction = "out"
match = { connectorId = 1 }
timeout_ms = 3000

[[steps]]
name = "CSMS accepts a DataTransfer"
action = "send_raw"
call = "DataTransfer"
payload = { vendorId = "com.example", messageId = "ping" }
expect = { status = "Accepted" }

[[steps]]
action = "stop_tx"
connector = 1
reason = "Local"
expect = { status = "Accepted" }

[[steps]]
action = "clear_rules"
```

Run it:

```bash
kirby-ocpp --url ws://localhost:9000/ocpp run --scenario session-with-rejected-reset.toml --report report.json --junit junit.xml
```

### Variables

An optional `[vars]` table at the top of the file declares variables (strings, numbers and booleans only; objects, arrays and dates are a load error). Names match `[A-Za-z_][A-Za-z0-9_]*`. `station_id` (the station's id) and `index` (its place in a fleet, 1 for a single station; `--first-index` plus the position in a fleet run) are built in and cannot be redefined.

```toml
name = "tagged-session"

[vars]
tag = "KIRBY-A"
wh = 500

[[steps]]
action = "data_transfer"
vendor_id = "com.example"
message_id = "hello-${station_id}"      # text inside a longer string
capture = { token = "/data" }          # store a value of the answer

[[steps]]
action = "send_raw"
call = "DataTransfer"
payload = { vendorId = "com.example", data = "${token}", messageId = "n${index}" }

[[steps]]
action = "authorize"
id_tag = "${tag}"
```

Rules:

- `${name}` is replaced right before the step runs, so a variable captured by an earlier step is visible. An unknown variable, a malformed `${` or a bad result fails that step with a message naming the variable.
- A string that is exactly `${name}` takes the variable's JSON type (`connectorId = "${n}"` sends a number when `n` is a number). Inside longer text the variable becomes text: strings without quotes, numbers and booleans as written. An object or array inside longer text is an error.
- `$${` is a literal `${`.
- Substituted fields: the JSON of `payload` and `match`, `expect.status`, `expect.payload`, and the string fields `id_tag`, `call`, `vendor_id`, `message_id`, `data`, `info`, `tech_info`, `event_type`, `rule`. Typed fields (`connector`, `reason`, `status`, `error_code`, `wh_delta`, `ms`, `timeout_ms`, `condition`, `direction`, `validate`) are never substituted: a variable cannot make a connector number or a timeout. A placeholder in `id_tag` is checked at load time as plain text (at most 20 characters), the substituted value again when the step runs. `rule`, `event_type` and `clear_rules` `call` with a placeholder are validated only when the step runs.
- `capture = { name = "/json/pointer" }` stores the value at a [JSON pointer](https://www.rfc-editor.org/rfc/rfc6901) of the step's raw answer after the step ran. It is valid on `send_raw`, `data_transfer` (the CALLRESULT payload) and `expect_call` (the payload of the matched CALL) and a load error elsewhere. An empty pointer captures the whole answer. A missing pointer fails the step. The captured value keeps its JSON type and may be an object or array (usable only as a whole `"${name}"` value).
- `kirby-ocpp run --var NAME=VALUE` (repeatable) overrides `[vars]`. `true`, `false` and numbers keep their type, everything else is a string. Setting `station_id` or `index`, or a bad name, is a usage error (exit 2). In a fleet run every station gets the same `--var` values and its own `index` and `station_id`.

### Loops

`action = "repeat"` runs nested steps several times. Nested steps are written as `[[steps.steps]]` (one more `steps` per nesting level) and are normal steps, including further `repeat` steps.

```toml
name = "plug-cycles"

[[steps]]
action = "repeat"
count = 3
name = "cycle"

  [[steps.steps]]
  action = "plug"
  connector = 1

  [[steps.steps]]
  action = "data_transfer"
  vendor_id = "demo"
  data = "cycle ${i} (${i0})"

  [[steps.steps]]
  action = "unplug"
  connector = 1
```

Rules:

- `count` is an integer from 1 to 1000. `steps` must not be empty. A `repeat` step takes only `action`, `count`, `var`, `steps` and `name`; `expect` and `capture` on it are load errors (put them on the nested steps). Unknown fields are load errors.
- Loops nest up to 3 levels deep. The scenario may run at most 10000 steps in total after expansion (a `repeat` with `count = 100` around 5 steps is 500); a larger scenario is refused when the file loads, with an error naming the scenario and the limit.
- Counters: inside a loop `${i}` is the 1-based and `${i0}` the 0-based iteration. A nested loop uses the same names and shadows the outer ones. Set `var = "round"` on a loop to name its counter `${round}` / `${round0}` instead, so an inner loop can still read the outer counter. Counters are ordinary variables, so an exact `"${i}"` keeps its number type; use text like `"TAG-${i}"` for an `id_tag`. Typed fields (`connector`, `ms`, ...) cannot use variables.
- Counter names are reserved: a `[vars]` entry or a `capture` name equal to a counter name used by any `repeat` of the scenario (`i`, `i0`, or the `var` name and its `0` twin) is a load error. A `var` must be a valid variable name and not `station_id` or `index`.
- `capture` inside a loop overwrites the variable on every iteration; later steps see the latest value.
- Report shape: loops are expanded, so the report, `--report` JSON and JUnit stay flat. The `repeat` step itself has no row; every executed step has one, named `<name> [<n>]` with the iteration path (`plug [2]`, in a nested loop `plug [2.3]`). `index` counts the expanded steps from 0, and the TUI Scenarios tab lists them the same way. Steps outside loops keep their plain name.
- A failing step stops the whole scenario: the rest of the iteration and all later iterations and steps are reported as `skipped` (never more rows than the 10000 step cap). Cancelling and `--timeout` behave as for flat steps: the step in flight fails with `cancelled`, the rest is skipped.

### Actions

Every step has `action` plus the fields of that action, and may add `name` (shown in reports; default is the action name) and `expect` (see [Expect](#expect)). Fields without a default are required.

| Action | Fields | What it does |
| --- | --- | --- |
| `connect`, `disconnect`, `reconnect` | none | Control the WebSocket |
| `boot` | none | Send BootNotification |
| `heartbeat` | none | Send Heartbeat |
| `plug`, `unplug` | `connector` (1 or more) | EV plugs in or out |
| `authorize` | `id_tag` | Send Authorize; implicitly expects `Accepted` |
| `start_tx` | `connector`, `id_tag` | Send StartTransaction; implicitly expects `Accepted` |
| `stop_tx` | `connector`, `reason` (default `Local`), optional `id_tag` | Send StopTransaction |
| `meter` | `connector`, `wh_delta` (default 1000) | Advance the meter and send MeterValues |
| `status` | `connector`, `status` | Set the connector status |
| `fault` | `connector`, `error_code`, optional `info` | Inject a fault (connector goes `Faulted`) |
| `clear_fault` | `connector` | Clear it |
| `sleep` | `ms` (at most 86400000) | Wait |
| `wait_for` | `condition`, `timeout_ms` (default 5000) | Wait until the station snapshot satisfies the condition |
| `expect_call` | `call`, optional `direction` (`in`/`out`) and `match`, `timeout_ms` (default 5000) | Wait for a CALL frame, see [expect_call](#expect_call) |
| `send_raw` | `call`, `payload` (default `{}`), `validate` (default `true`) | Send any CALL; `validate = false` skips the schema check so malformed requests can be tested |
| `data_transfer` | `vendor_id`, optional `message_id`, `data` | Send DataTransfer |
| `set_rule` | `rule` | Add a [response rule](emulator.md#response-rules), for example `"Reset=reject*2"` |
| `clear_rules` | optional `call` | Remove the rule of one command, or all rules when omitted |
| `request_certificate` | none | Security Whitepaper: key pair, CSR and `SignCertificate` |
| `repeat` | `count`, optional `var`, nested `steps` | Run the nested steps `count` times, see [Loops](#loops) |
| `security_event` | `event_type`, optional `tech_info` | Send SecurityEventNotification; the event type (case-insensitive, one of the whitepaper's 21 names) is checked when the file loads |

Value sets:

* `status` and `wait_for` connector statuses: `Available`, `Preparing`, `Charging`, `SuspendedEVSE`, `SuspendedEV`, `Finishing`, `Reserved`, `Unavailable`, `Faulted`.
* `error_code`: `ConnectorLockFailure`, `EVCommunicationError`, `GroundFailure`, `HighTemperature`, `InternalError`, `LocalListConflict`, `NoError`, `OtherError`, `OverCurrentFailure`, `PowerMeterFailure`, `PowerSwitchFailure`, `ReaderFailure`, `ResetFailure`, `UnderVoltage`, `OverVoltage`, `WeakSignal`.
* `reason`: `EmergencyStop`, `EVDisconnected`, `HardReset`, `Local`, `Other`, `PowerLoss`, `Reboot`, `Remote`, `SoftReset`, `UnlockCommand`, `DeAuthorized`.

Steps that send a CALL (`start_tx`, `send_raw`, ...) finish when the CSMS has answered or the call timed out (`--call-timeout-ms`).

### wait_for conditions

`condition = { kind = "...", ... }`:

| `kind` | Extra fields | Holds when |
| --- | --- | --- |
| `connected` | none | The WebSocket is connected |
| `disconnected` | none | The WebSocket is disconnected |
| `registration` | `status` (`Accepted`, `Pending`, `Rejected`) | The last BootNotification got that status |
| `connector_status` | `connector`, `status` | The connector has that status |
| `tx_active` | `connector` | A transaction acknowledged by the CSMS is running |
| `tx_idle` | `connector` | No transaction on the connector |
| `no_pending_messages` | none | No transaction messages are queued for delivery |

`timeout_ms` must be between 1 and 86400000. The step fails on timeout.

### expect_call

Waits for a CALL frame, sent (`direction = "out"`) or received (`"in"`), whose action is `call` and whose payload contains `match` (a subset: objects match by key, arrays element by element with equal length, everything else by equality). Without `direction` either direction matches.

The CALLs it considers are those seen since the previous matched `expect_call` (or the scenario start), so a CALL sent before the step still counts. A match consumes everything up to it; later `expect_call` steps cannot match the same CALL. The runner remembers only CALLs some `expect_call` names, and keeps a payload only if some step uses `match`. At most 10 000 unmatched CALLs (about 16 MiB) are kept.

If events were lost because the runner fell behind and the wanted CALL is not among the frames seen before the loss, the step fails with "the scenario runner fell behind"; it reports the loss once, so it does not poison later `expect_call` steps.

### Expect

`expect = { status = "...", payload = {...}, error = true }` asserts the outcome of a step that sends a command. All keys are optional:

| Key | Meaning |
| --- | --- |
| `status` | The `status` of the answer must equal this (for `authorize`, the idTag status; for `start_tx`, `Accepted` or the rejection status) |
| `payload` | The CALLRESULT payload must contain this subset. Only steps that return the raw answer have one, for example `send_raw` and `data_transfer`; elsewhere the step fails |
| `error` | The step must fail with a station error (for example a CALLERROR or a timeout). If it succeeds the step fails |

`authorize` and `start_tx` imply `status = "Accepted"` unless the step sets another status, so a rejected tag stops the scenario; set `expect = { status = "Blocked" }` to test rejection. `expect` on `sleep`, `wait_for` and `expect_call` is accepted but has no effect.

### Built-in scenarios

`kirby-ocpp scenarios list` lists them, `kirby-ocpp scenarios show NAME` prints one as the TOML that runs. Use `--scenario NAME` with `run`, or the Scenarios tab in the [UI](emulator.md#scenarios-tab).

| Name | What it does |
| --- | --- |
| `charge-session` | Plug in, Authorize, StartTransaction, MeterValues (connector 1) |
| `finish-session` | MeterValues, StopTransaction, unplug (needs a running transaction on connector 1) |
| `offline-sync` | Go offline, run a whole session, reconnect, wait until the queued transaction messages are delivered, unplug |
| `fault-recovery` | Inject a fault on connector 1, then clear it |
| `heartbeat-and-status` | Heartbeat, connector 1 Unavailable and back to Available, plug and unplug; asserts each StatusNotification |
| `metering-burst` | Plug in, start a transaction, three MeterValues (each asserted), stop, unplug |
| `data-transfer` | DataTransfer with the variable `vendor_id` (default `kirby`, override with `--var vendor_id=...`); any DataTransfer status passes |
| `security-event` | SecurityEventNotification `StartupOfTheDevice`, asserts the CALL went out |
| `reconnect` | Disconnect, wait until down, connect, wait for an accepted registration, Heartbeat |

The new built-ins need no prior state (connector 1 Available) and only assert CALLs sent by the station, so they pass against any CSMS that answers `Accepted` (or any status, for `data-transfer`).

Built-ins target connector 1; `--connector N` retargets every connector-bound step of a built-in (files are never retargeted). `--id-tag TAG` replaces the idTag of `authorize`, `start_tx` and `stop_tx` steps (the latter only where the step sets one) in built-ins and files alike. `offline-sync` works on a stock station: `LocalAuthorizeOffline` and `AllowOfflineTxForUnknownId` default to `true`; switch them off with `ChangeConfiguration` to test the CSMS-side rejection path (the offline start is then rejected and the scenario fails).

## Running scenarios

```
kirby-ocpp [station options] run --scenario NAME_OR_FILE [--report report.json] [--junit junit.xml] [--timeout 60] [--id-tag TAG]
```

`--scenario` is a built-in name or a file path (a built-in name wins). `run` connects, waits for an accepted BootNotification, runs the steps and prints the report. Station events (frames and log lines) are also printed to stdout while it runs; `--log-file` keeps them in a file. The station options (`--url`, `--id`, TLS flags, ...) are in [configuration.md](configuration.md).

| Option | Meaning |
| --- | --- |
| `--scenario NAME_OR_FILE` | Required. Built-in name or TOML file |
| `--report PATH` | Write the report as JSON |
| `--junit PATH` | Write a JUnit XML report |
| `--timeout SECONDS` | Overall limit, connecting included (default 60). On expiry the running step fails with `cancelled` and the rest are skipped |
| `--id-tag TAG` | Replace the idTag in the scenario |
| `--json` | Print events and the report as JSON lines |

A bad scenario file (parse error, unknown field) exits 2 before any connection. If the station cannot connect or the BootNotification is not accepted before `--timeout`, there is no step report: the error goes to stderr, no `--report` file is written, and the JUnit file (if requested) holds one errored `setup` case. The exit code is 1.

| Code | Meaning |
| --- | --- |
| 0 | Every step passed |
| 1 | A step failed, the run timed out, a setup error, or a report file could not be written |
| 2 | Usage or configuration error, including an invalid scenario file |
| 130 | Interrupted by SIGINT. No JUnit file is written. SIGTERM ends the run with 1 and still writes the JUnit file |

### CI example

```bash
kirby-csms --listen 127.0.0.1:9000 --no-tui --log-file csms.jsonl &
CSMS=$!
kirby-ocpp --url ws://127.0.0.1:9000/ocpp run --scenario charge-session --junit junit.xml
status=$?
kill $CSMS
exit $status
```

Point `--junit` at the path your CI reads test reports from.

## Suite runs

```
kirby-ocpp [station options] suite [SOURCE...] [--all-builtins] [--report suite.json] [--junit junit.xml] [--timeout 60] [--fail-fast] [--var NAME=VALUE]
```

`suite` runs many scenarios one after another, each on a fresh station (see [Suite mode](emulator.md#suite-mode)). SOURCE is a file, a directory (`*.toml` directly inside, sorted by name; an empty directory is a usage error) or a built-in name; at least one SOURCE or `--all-builtins` is required. `--all-builtins` skips `finish-session`, which needs a running transaction. Scenario names that appear twice get their source appended in the output and the reports (`name (path)`). All scenarios are loaded first: a bad file exits 2 before any connection.

| Option | Meaning |
| --- | --- |
| `--timeout SECONDS` | Per scenario, connecting included (default 60) |
| `--fail-fast` | Stop after the first failed scenario; the rest are reported as SKIP |
| `--var NAME=VALUE` | Variable for every scenario, as in `run` |
| `--report PATH` | Write the suite summary as JSON |
| `--junit PATH` | Write one JUnit file with a `<testsuite>` per scenario (skipped scenarios are left out; a scenario whose station never booted is one errored `setup` case) |
| `--json` | Print events and one `report` line per scenario that ran, then the summary |

Output: `PASS name (1.2 s)`, `FAIL name: first failing step - reason` (or the setup error), `SKIP name`, then `suite: 7/8 scenarios passed, FAILED: a, b` (`, SKIPPED: ...` when some were skipped). The JSON summary (`--report`, and the last line under `--json`) is `{"type":"suite_report","passed":false,"total":8,"passed_count":7,"failed":["a"],"skipped":[],"scenarios":[{"name":"a","status":"failed","duration_ms":1200,"report":{...}}],"duration_ms":9000}`. `status` is `passed`, `failed`, `error` (never booted, or cut short; carries `error` instead of `report`) or `skipped`.

Exit codes: 0 only if every scenario passes; 1 if any fails, times out or cannot boot, or a report file could not be written; 2 usage error (no source, empty directory, bad file, `--count` above 1); 130 SIGINT (no report files). SIGTERM stops the running scenario, skips the rest, writes the reports and exits 1.

A CI job that tests the CSMS under test with the emulator, and the emulator with a scripted CSMS:

```bash
kirby-csms --listen 127.0.0.1:9000 --no-tui --script csms-script.toml --junit csms-junit.xml &
CSMS=$!
kirby-ocpp --url ws://127.0.0.1:9000/ocpp suite scenarios/ --junit junit.xml --report suite.json
status=$?
wait $CSMS || status=1
exit $status
```

## Fleet runs

With `--count N` (above 1) the same scenario runs on all N stations at once; see [Fleet mode](emulator.md#fleet-mode) for ids and staggering.

```bash
kirby-ocpp --url ws://localhost:9000/ocpp --id 'CP-{n}' --count 20 --stagger-ms 50 \
  run --scenario charge-session --report out.json --junit fleet.xml
```

* One report per station, each line tagged `[CP-1] ...` (JSON reports carry a `"station"` field), then one aggregate line: `fleet: 3/3 stations PASSED`, or `fleet: 2/3 stations passed, FAILED: CP-2`.
* With `--json` the aggregate is `{"type":"fleet_report","passed":true,"total":3,"passed_count":3,"failed":[],"stations":[{"station":"CP-1","passed":true}, ...]}`. A station that never booted has `"passed": false` and an `"error"` text.
* `--timeout` covers the whole fleet, stagger included.
* `--report out.json` writes one file per station: `out.CP-1.json`. Characters of the id other than letters, digits, `-` and `_` become `_`, so ids that differ only in such characters would share a file.
* `--junit fleet.xml` writes one file for the whole fleet, with a suite named `scenario [CP-1]` per station.
* Exit code: 0 only if every station passes, 1 if any fails or times out, 2 usage error, 130 SIGINT. After an interruption no report files are written.

## CSMS scripts

`kirby-csms --script FILE` starts the server, runs the script against the stations that connect, prints the report and exits with its verdict. The stations are external processes (for example `kirby-ocpp`) that connect while the script waits. Steps run in order and the first failing step stops the run (later steps are `SKIP`). Top-level keys are `name` (required, not empty), an optional `[vars]` table (see [Variables, capture and payload checks](#script-variables-capture-and-payload-checks)) and `[[steps]]` (at least one); other top-level keys are ignored. Unknown fields inside a step are rejected, naming the step.

### Example

```toml
name = "smoke"

[[steps]]
action = "respond"                # answer every DataTransfer with this CALLRESULT
call = "DataTransfer"
payload = { status = "Accepted" }

[[steps]]
action = "wait_station"
station = "CP-1"
booted = true                     # also wait for its BootNotification
timeout_ms = 20000

[[steps]]
action = "wait_call"
call = "StartTransaction"
payload_contains = { connectorId = 1 }
timeout_ms = 20000

[[steps]]
name = "heartbeat interval accepted"
action = "call"
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "HeartbeatInterval", value = "60" }
expect_status = "Accepted"

[[steps]]
name = "unknown command is refused"
action = "call"
station = "CP-1"
call = "Nonsense"
expect_error = true
```

Run it with a station in another process:

```bash
kirby-csms --listen 127.0.0.1:9000 --script smoke.toml --report report.json --junit junit.xml --timeout 120 &
kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id CP-1 run --scenario charge-session
wait
```

### Steps

Every step takes an optional `name`. `timeout_ms` must be between 1 and 86400000.

| Action | Fields | What it does |
| --- | --- | --- |
| `wait_station` | optional `station`, `count`, `booted` (default `false`), `timeout_ms` (default 30000) | Wait until `station` is connected and/or at least `count` stations are (default: 1). `booted = true` also needs the BootNotification, not just the open socket |
| `wait_call` | `call`, optional `station`, `payload_contains`, `capture`, `timeout_ms` (default 5000) | Wait for an inbound CALL (from a given station, containing a payload subset); `capture` stores values of the matched payload |
| `call` | `station`, `call`, `payload` (default `{}`), `expect_status`, `expect_payload`, `expect_error`, `capture`, `timeout_ms` (default 5000) | Send a CALL to a connected station and check the answer. `expect_status` checks `payload.status`; `expect_payload` checks a payload subset (see below); `expect_error = true` wants a CALLERROR. `expect_error` together with `expect_status`, `expect_payload` or `capture` is a script error. Without either, any CALLRESULT passes and a CALLERROR fails. Payloads are sent as given, not schema-checked |
| `sleep` | `ms` | Wait (at most 86400000) |
| `respond` | `call`, `payload` | Answer every inbound CALL of that action with this CALLRESULT payload |
| `clear_respond` | `call` | Remove that responder |
| `inject_fault` | `call`, `kind`, `payload`, `times` (default 1) | Make the next `times` CALLs of that action fail; see below |

`wait_call` consumes the CALL it matches: each inbound CALL satisfies at most one `wait_call`, but waits may come in any order (a later wait can match a CALL that arrived before the previous wait's match). CALLs that arrive while other steps run are kept (the last 10 000, at most about 16 MiB); only CALLs some `wait_call` of the script names are remembered, and their payload only if a `wait_call` for that action uses `payload_contains` or `capture`. If the event stream lagged (events were lost), a `wait_call` that finds no match fails at once with a lost-events error instead of waiting out its timeout.

### Script variables, capture and payload checks

Scripts have the same variables as [emulator scenarios](#variables), with these differences: only the variables you define exist (no `${station_id}` or `${index}`: a script talks to many stations), and `capture` also feeds later steps.

- `[vars]` at the top level declares variables (strings, numbers, booleans). `kirby-csms --script FILE --var NAME=VALUE` (repeatable, needs `--script`, a bad name is a usage error, exit 2) overrides or adds one; `true`/`false`/numbers keep their type.
- `${name}` is replaced when the step runs, in `station`, `expect_status` and the JSON of `payload`, `payload_contains`, `expect_payload` and the payloads of `respond` and `inject_fault` (object keys are not substituted). A string that is exactly `"${name}"` keeps the variable's JSON type; `$${` is a literal `${`. Action names (`call`, also in `wait_call`, `respond`, `clear_respond`, `inject_fault`) and typed fields (`ms`, `times`, `count`, `booted`, `timeout_ms`, `expect_error`) are not substituted. An unknown variable fails that step with a reason naming it.
- `capture = { name = "/json/pointer" }` on `call` stores a value of the CALLRESULT payload, on `wait_call` a value of the matched CALL payload (the payload is then kept even without `payload_contains`). The empty pointer `""` captures the whole payload. A missing pointer fails the step. Names and pointers are checked when the script loads.
- `expect_payload = { ... }` on `call`: the CALLRESULT payload must contain this JSON, with the same subset rule as `payload_contains` (objects by key, arrays element by element and equal length, anything else equal). It works together with `expect_status`; the order of checks is status, payload, capture.

```toml
name = "echo the tag"

[vars]
interval = "60"

[[steps]]
action = "wait_station"
station = "CP-1"
booted = true

[[steps]]
action = "wait_call"
call = "Authorize"
capture = { tag = "/idTag" }

[[steps]]
action = "call"
station = "CP-1"
call = "DataTransfer"
payload = { vendorId = "acme", data = "tag-${tag}" }
expect_payload = { status = "UnknownVendorId" }

[[steps]]
action = "call"
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "HeartbeatInterval", value = "${interval}" }
expect_status = "Accepted"
capture = { result = "/status" }
```

`inject_fault` kinds:

| `kind` | Effect on the station's CALL |
| --- | --- |
| `call_error` | Answer with CALLERROR `InternalError` ("injected CALLERROR") |
| `silence` | Never answer |
| `close` | Close the connection |
| `invalid_payload` | Answer with a CALLRESULT carrying the given `payload` as is (`payload` is required; use it to send schema-invalid data) |

The default step names in reports are `wait_station CP-1`, `wait_call StartTransaction`, `call CP-1 ChangeConfiguration`, `sleep 100 ms`, `respond DataTransfer`, and so on.

The whole run is bounded by `--timeout` (default 60 s, 1 to 86400): the step running at that moment fails with `the script --timeout was reached` and the rest are skipped. Other `kirby-csms` flags (`--listen`, TLS, security profile, `--json`, ...) are in [configuration.md](configuration.md).

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | The script passed |
| 1 | A step failed or timed out, the server could not start, or a report file could not be written. On SIGTERM the `--junit` file still gets one failed suite (`interrupted`) |
| 2 | Usage, configuration or script error |
| 130 | Interrupted by SIGINT, no report |

## Reports

Both runners produce the same report structure, so tools can read either.

### JSON (`--report`)

`--report PATH` writes pretty-printed JSON. Step `status` is `passed`, `failed` (with a `reason`) or `skipped`; `index` is 0-based; durations are in milliseconds.

```json
{
  "scenario": "smoke",
  "passed": false,
  "cancelled": false,
  "steps": [
    { "index": 0, "name": "respond DataTransfer", "status": "passed", "duration_ms": 0 },
    { "index": 1, "name": "wait_station CP-1", "status": "passed", "duration_ms": 412 },
    { "index": 2, "name": "wait_call StartTransaction", "status": "failed", "reason": "no matching StartTransaction CALL within 20000 ms", "duration_ms": 20001 },
    { "index": 3, "name": "heartbeat interval accepted", "status": "skipped", "duration_ms": 0 }
  ],
  "duration_ms": 20413
}
```

`cancelled` is true when the run was stopped (timeout or signal). With `--json` the same object is printed on stdout as one line with an extra `"type": "report"` (and `"station"` in fleet runs). Without `--json` the text form is:

```
scenario smoke
  PASS  1 respond DataTransfer (0 ms)
  PASS  2 wait_station CP-1 (412 ms)
  FAIL  3 wait_call StartTransaction (20001 ms): no matching StartTransaction CALL within 20000 ms
  SKIP  4 heartbeat interval accepted (0 ms)
FAILED in 20413 ms
```

(`kirby-csms` prints `script <name>` as the first line.)

### JUnit XML (`--junit`)

One `<testsuite>` per run (a fleet run has one per station, named `scenario [CP-1]`), one `<testcase>` per step named `<number>. <step name>` with the suite name as `classname`. A failed step has a `<failure>`, a skipped one `<skipped/>`. A run that never produced a report (the station could not connect or boot) is a suite with one `setup` test case holding an `<error>`.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<testsuites tests="4" failures="1" errors="0" skipped="1" time="20.413">
  <testsuite name="smoke" tests="4" failures="1" errors="0" skipped="1" time="20.413">
    <testcase classname="smoke" name="1. respond DataTransfer" time="0.000"/>
    <testcase classname="smoke" name="2. wait_station CP-1" time="0.412"/>
    <testcase classname="smoke" name="3. wait_call StartTransaction" time="20.001">
      <failure message="no matching StartTransaction CALL within 20000 ms" type="StepFailed">no matching StartTransaction CALL within 20000 ms</failure>
    </testcase>
    <testcase classname="smoke" name="4. heartbeat interval accepted" time="0.000">
      <skipped/>
    </testcase>
  </testsuite>
</testsuites>
```

Setup error:

```xml
<testcase classname="charge-session" name="setup" time="0.000">
  <error message="timed out waiting for the central system to accept the BootNotification" type="SetupError">...</error>
</testcase>
```

Text from a station (CALLERROR descriptions) can end up in step names and reasons; it is sanitised for the terminal and escaped for XML.

## Related pages

* [emulator.md](emulator.md): the emulator, fleet mode, response rules, TUI.
* [configuration.md](configuration.md): flags and environment variables of both binaries.
* [security.md](security.md): security profiles and certificates (for `request_certificate` and `security_event`).
* [architecture.md](architecture.md): scenario runner and script engine in the code.
