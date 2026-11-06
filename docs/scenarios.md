# Scenarios and scripts

Two TOML formats drive a test run without a human:

* an emulator **scenario** drives one or more `kirby-ocpp` stations against a CSMS under test ([Emulator scenarios](#emulator-scenarios));
* a `kirby-csms` **script** drives the central system and waits for stations that connect ([CSMS scripts](#csms-scripts)).

Both stop at the first failing step, print a PASS/FAIL/SKIP report, can write a JSON report and a JUnit XML file, and use the same exit codes ([Reports](#reports)).

Contents: [Emulator scenarios](#emulator-scenarios) · [Running scenarios](#running-scenarios) · [Fleet runs](#fleet-runs) · [CSMS scripts](#csms-scripts) · [Reports](#reports) · [Related pages](#related-pages)

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

`kirby-csms --script FILE` starts the server, runs the script against the stations that connect, prints the report and exits with its verdict. The stations are external processes (for example `kirby-ocpp`) that connect while the script waits. Steps run in order and the first failing step stops the run (later steps are `SKIP`). Top-level keys are `name` (required, not empty) and `[[steps]]` (at least one); other top-level keys are ignored. Unknown fields inside a step are rejected, naming the step.

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
| `wait_call` | `call`, optional `station`, `payload_contains`, `timeout_ms` (default 5000) | Wait for an inbound CALL (from a given station, containing a payload subset) |
| `call` | `station`, `call`, `payload` (default `{}`), `expect_status`, `expect_error`, `timeout_ms` (default 5000) | Send a CALL to a connected station and check the answer. `expect_status` checks `payload.status`; `expect_error = true` wants a CALLERROR. Both together are a script error. Without either, any CALLRESULT passes and a CALLERROR fails. Payloads are sent as given, not schema-checked |
| `sleep` | `ms` | Wait (at most 86400000) |
| `respond` | `call`, `payload` | Answer every inbound CALL of that action with this CALLRESULT payload |
| `clear_respond` | `call` | Remove that responder |
| `inject_fault` | `call`, `kind`, `payload`, `times` (default 1) | Make the next `times` CALLs of that action fail; see below |

`wait_call` consumes the CALL it matches: each inbound CALL satisfies at most one `wait_call`, but waits may come in any order (a later wait can match a CALL that arrived before the previous wait's match). CALLs that arrive while other steps run are kept (the last 10 000, at most about 16 MiB); only CALLs some `wait_call` of the script names are remembered, and their payload only if a `wait_call` for that action uses `payload_contains`. If the event stream lagged (events were lost), a `wait_call` that finds no match fails at once with a lost-events error instead of waiting out its timeout.

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
