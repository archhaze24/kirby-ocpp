# Scenarios and scripts

Two TOML formats drive a test run without a human:

* an emulator **scenario** drives one or more `kirby-ocpp` stations against a CSMS under test ([Emulator scenarios](#emulator-scenarios));
* a `kirby-csms` **script** drives the central system and waits for stations that connect ([CSMS scripts](#csms-scripts)).

Both stop at the first failing step, print a PASS/FAIL/SKIP report, can write a JSON report and a JUnit XML file, and use the same exit codes ([Exit codes](configuration.md#exit-codes)).

Contents: [Emulator scenarios](#emulator-scenarios) · [Running scenarios](#running-scenarios) · [Suite runs](#suite-runs) · [Fleet runs](#fleet-runs) · [CSMS scripts](#csms-scripts) · [Reports](#reports) · [Related pages](#related-pages)

## Emulator scenarios

A scenario file has a `name`, an optional `description` and a list of `[[steps]]`. Unknown fields anywhere are rejected when the file is loaded, with the step number, the file position and, for a likely typo, a suggestion in the message (`scenario.toml:12:1: invalid scenario: step 3: action `heartbeat`: unknown variant `heartbaet`, ...; did you mean `heartbeat`?`). A scenario needs at least one step.

### File header keys

The allowed top-level keys are `name`, `description`, `vars`, `steps` and three keys that describe the file itself:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `ocpp_version` | string | `"1.6"` | Protocol the file is written for. Any other value is a load error (`this build supports OCPP 1.6 only`, naming the value). |
| `schema` | integer | `1` | Version of the file format. Any other value is a load error (`unsupported schema N`). |
| `tags` | array of strings | none | Free-form labels. Each entry must be a string (any other type is a load error); the text is not checked further. `suite --tag` and `--skip-tag` select on them (exact, case-sensitive match); built-in scenarios have none. |

Both CSMS scripts and emulator scenarios take these keys and reject every other top-level key. The version keys are read and checked before anything else, so a file written for a later version fails with `unsupported schema N` or `this build supports OCPP 1.6 only` even when it also uses keys this build does not know. A value of the wrong type fails with a plain message such as `` `schema` must be an integer``.

Reserved step keys: `evse` and `id_token` are reserved for scenarios that target OCPP 2.0.1 and later. Any step that uses them fails to load with ``action `x`: `evse` is reserved for OCPP 2.x steps; not supported in this build``.

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
- A string that is exactly `${name}` takes the variable's JSON type (`connectorId = "${n}"` sends a number when `n` is a number). Inside longer text the variable becomes text: strings without quotes, booleans as `true` or `false`, and numbers as written on the command line (`--var n=1.50` gives `1.50`, `--var n=1e3` gives `1e3`); a number declared in `[vars]` prints in its canonical form (`1.5`). An exact `"${n}"` still sends the number as a JSON number. An object or array inside longer text is an error.
- `$${` is a literal `${`.
- Substituted fields: the JSON of `payload` and `match`, `expect.status`, `expect.payload`, and the string fields `id_tag`, `call`, `vendor_id`, `message_id`, `data`, `info`, `tech_info`, `event_type`, `rule`.
  - Typed fields (`connector`, `reason`, `status`, `error_code`, `wh_delta`, `ms`, `timeout_ms`, `condition`, `direction`, `validate`) are never substituted: a variable cannot make a connector number or a timeout.
  - A placeholder in `id_tag` is checked at load time as plain text (at most 20 characters), the substituted value again when the step runs.
  - `rule`, `event_type` and `clear_rules` `call` with a `${name}` placeholder are validated only when the step runs; text that only contains `$${` escapes is validated at load time after unescaping. One substitution may produce at most about 1 MiB (serialized); a larger result fails the step.
  - Values that come from the CSMS (captured variables, answers) are cut to about 200 characters in failure reasons.
- `capture = { name = "/json/pointer" }` stores the value at a [JSON pointer](https://www.rfc-editor.org/rfc/rfc6901) of the step's raw answer after the step ran. It is valid on `send_raw`, `data_transfer` (the CALLRESULT payload) and `expect_call` (the payload of the matched CALL) and a load error elsewhere. An empty pointer captures the whole answer. A missing pointer fails the step. The captured value keeps its JSON type and may be an object or array (usable only as a whole `"${name}"` value).
- `kirby-ocpp run --var NAME=VALUE` (repeatable) overrides `[vars]`. `true`, `false` and numbers keep their type, everything else is a string. Setting `station_id` or `index`, or a bad name, is a usage error (exit 2), and so is a name the scenario never uses (see `run` below). In a fleet run every station gets the same `--var` values and its own `index` and `station_id`.

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

Every step has `action` plus the fields of that action, and may add `name` (shown in reports; the default report label is derived from the action, see [JUnit XML](#junit-xml---junit); an empty or blank `name` counts as absent) and `expect` (see [Expect](#expect)). Fields without a default are required.

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
| `error` | The step must fail with a station error (for example a CALLERROR or a timeout). If it succeeds the step fails. Combined with `status` or `payload` it is a load error (`error = true` excludes them) |

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

```bash
kirby-ocpp [station options] run --scenario NAME_OR_FILE [--report report.json] [--junit junit.xml] [--timeout 60] [--id-tag TAG]
```

`--scenario` is a file path or a built-in name. An argument with a path separator, ending in `.toml`, or naming an existing file or directory is a path (a built-in only applies when no such path exists). Scenario files must be regular files of at most 1 MiB (FIFOs and devices are refused).
- A `--var` for a name the scenario neither declares in `[vars]`, uses as a loop counter, nor references as `${name}` is a usage error (exit 2, "unknown variable"). `run` connects, waits for an accepted BootNotification, runs the steps and prints the report.
- Frame and log lines are not printed by default (only the report); `-v` prints them as they happen, `-q` leaves only the verdict line, `--json` prints events and the report as JSON lines, and `--log-file` keeps the full log in a file (see [configuration.md](configuration.md#output-levels)). A `run` is ephemeral: it reads and writes no state file unless `--persist` is given. The station options (`--url`, `--id`, TLS flags, ...) are in [configuration.md](configuration.md).

| Option | Meaning |
| --- | --- |
| `--scenario NAME_OR_FILE` | Required. Built-in name or TOML file |
| `--report PATH` | Write the report as JSON |
| `--junit PATH` | Write a JUnit XML report |
| `--timeout SECONDS` | Overall limit, connecting included (default 60). On expiry the running step fails with `cancelled` and the rest are skipped |
| `--id-tag TAG` | Replace the idTag in the scenario |
| `--json` | Print events and the report as JSON lines |
| `--persist` | Keep station state in the state file (a `run` is ephemeral by default) |
| `-v`, `-q` | Also print the event lines / print only the verdict line |

A bad scenario file (parse error, unknown field) exits 2 before any connection. If the station cannot connect or the BootNotification is not accepted before `--timeout`, there is no step report: the error goes to stderr, no `--report` file is written, and the JUnit file (if requested) holds one errored `setup` case. The exit code is 1.

Exit codes: 0 when every step passes, 1 for a failed step, timeout, setup error or unwritable report, 2 for a usage error, 130 for SIGINT. The full table is in [configuration.md](configuration.md#kirby-ocpp-run).

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

```bash
kirby-ocpp [station options] suite [SOURCE...] [--all-builtins] [--report suite.json] [--junit junit.xml] [--timeout 60] [--fail-fast] [--var NAME=VALUE]
  [--filter GLOB] [--skip GLOB] [--tag TAG] [--skip-tag TAG] [--retries N] [--jobs N]
  [--github-annotations] [--summary-md summary.md]
```

`suite` runs many scenarios, by default one after another, each on a fresh station (see [Suite mode](emulator.md#suite-mode)).
- SOURCE is a file, a directory (`*.toml` directly inside, sorted by name; an empty directory is a usage error) or a built-in name (an existing path or an argument that looks like a path wins over a built-in of the same name); at least one SOURCE or `--all-builtins` is required.
- A file named twice, or a file plus its directory, runs once (by canonical path). A `--var` that a scenario does not use is ignored with a `warning:` line on stderr per scenario. `--all-builtins` skips `finish-session`, which needs a running transaction.
- Scenario names that appear twice get their source appended in the output and the reports (`name (path)`). All scenarios are loaded first: a bad file exits 2 before any connection.

| Option | Meaning |
| --- | --- |
| `--timeout SECONDS` | Per scenario, connecting included (default 60) |
| `--fail-fast` | Stop after the first failed scenario; the rest are reported as SKIP |
| `--var NAME=VALUE` | Variable for every scenario, as in `run` |
| `--report PATH` | Write the suite summary as JSON |
| `--junit PATH` | Write one JUnit file with a `<testsuite>` per scenario (skipped scenarios are left out; a scenario whose station never booted is one errored `setup` case) |
| `--json` | Print events and one `report` line per scenario that ran, then the summary |
| `--filter GLOB`, `--skip GLOB`, `--tag TAG`, `--skip-tag TAG` | Select scenarios, see [Selecting scenarios](#selecting-scenarios) |
| `--retries N` | Run a failed scenario again, see [Retries](#retries-and-flaky-scenarios) |
| `--jobs N` | Run scenarios in parallel, see [Parallel jobs](#parallel-jobs) |
| `--github-annotations`, `--summary-md PATH` | CI output, see [GitHub annotations and summary](#github-annotations-and-summary) |

#### Selecting scenarios

The selection applies to the loaded scenarios (every source is loaded and validated first) in this order:

1. `--filter GLOB` (repeatable): keep scenarios whose `name` matches any glob. Without `--filter` everything is kept. `*` stands for any run of characters (also none), `?` for exactly one, everything else matches itself, case-sensitively. The glob is matched against the scenario `name`, not the file name; a glob that contains a comma is one glob (the flag is not comma-separated).
2. `--skip GLOB` (repeatable): drop scenarios whose name matches any glob.
3. `--tag TAG` (repeatable): keep scenarios whose top-level `tags` contain every given tag. A scenario without tags never matches a `--tag`.
4. `--skip-tag TAG` (repeatable): drop scenarios that carry any given tag.

Scenarios left out are not part of the run: they do not appear in the output or the reports, and `total` counts only the selected ones. The order of the remaining scenarios is the order of the sources. A selection that leaves nothing exits 2 with `the selection ... matches none of the N scenarios`; nothing connects. `--fail-fast` and `--var` warnings apply to the selected scenarios.

```bash
kirby-ocpp --url ws://127.0.0.1:9000/ocpp suite scenarios/ --tag smoke --skip-tag slow --skip 'wip-*'
```

#### Retries and flaky scenarios

`--retries N` (0 to 10, default 0) runs a scenario that failed, timed out or could not boot again, up to N more times. Every attempt gets a fresh station (so with the same station id); a retry never starts after a signal. The scenario `status` is the status of the last attempt: it is `passed` when a retry passed and `failed` or `error` when every attempt failed. A scenario that passed after at least one failed attempt is flaky; it counts as passed and does not change the exit code, but it is visible:

- Text output: `RETRY name: reason (attempt 1 of 3)` after each failed attempt that is followed by another, `PASS name (1.2 s, flaky: passed on attempt 2)` for the pass, `FAIL name: reason (after 3 attempts)` when the last attempt failed too, and `, FLAKY: a, b` in the summary line.
- JSON summary: new top-level `retries` (the `--retries` value) and `flaky_names`; every `scenarios` entry gains `attempts` (attempts made, `0` for a skipped scenario), `flaky` (boolean) and `attempt_results` (one `{attempt, status, duration_ms, message}` per attempt, oldest first; `message` only for a failure). `duration_ms`, `started_at` (first attempt) and `report` describe the last attempt only, and `counts` count final statuses.
- JUnit: with `--retries` above 0 every `<testsuite>` gets the property `attempts`, and `flaky` with value `true` for a flaky pass. Each failed earlier attempt becomes a `<flakyFailure>` (a step failure) or `<flakyError>` (the station never booted) on the test case of its first failed step, or on the first case; when the final attempt failed too, the same data is written as `<rerunFailure>` / `<rerunError>`. These are the Maven Surefire elements, which common CI reporters turn into "flaky" markers. Test case names, `classname` and the counts describe the last attempt, so history by name keeps working.
- `--github-annotations` prints a `::warning` for a flaky pass.

#### Parallel jobs

`--jobs N` (1 to 16, default 1) runs up to N scenarios at the same time; with fewer scenarios than jobs only that many slots are used. Each scenario still runs on its own station with no state on disk. Slot 1 uses the station id as given, slot 2 the id with `-2` appended, slot 3 `-3`, and so on, so two running stations never share an id and a central system sees distinct charge points (`CP-1`, `CP-1-2`, ...). A central system that allows only listed ids must list them. Scenarios are started in selection order as slots free up.

- Reports and summaries list the scenarios in the order of the selection whatever order they finish in. Only the progress lines (`PASS`, `FAIL`, `RETRY`) appear in completion order; every line is written whole. `-v` event lines of parallel stations interleave line by line.
- Each scenario keeps its own `--timeout`. `--fail-fast` stops starting new scenarios after the first final failure; scenarios already running finish.

#### GitHub annotations and summary

`--github-annotations` prints one workflow command per failed scenario and one per flaky pass to stdout, after the last scenario finished and before the `suite:` line, in selection order:

```text
::error file=scenarios/bad.toml,title=bad::authorize - expected Accepted, got Blocked
::warning file=scenarios/wobbly.toml,title=wobbly::flaky: passed on attempt 2 after a failure (heartbeat - timeout)
```

`file` is the scenario file (omitted for built-ins), `title` the scenario name and the message the first failing step and its reason (the setup error for a scenario that never booted). `%`, CR and LF are escaped as the workflow command syntax requires, `:` and `,` also in `file` and `title`; control characters and invisible marks in station-controlled text are written as `\u{XXXX}`. The flag is a plain switch instead of a `--format` value because `--format` is reserved for choosing the report format of every command; the workflow commands are extra lines next to the usual output. It cannot be combined with `--json` (exit 2): a JSON stream must stay machine-readable.

`--summary-md PATH` writes a Markdown summary: a headline (`7 of 8 scenarios passed, 1 failed, 1 flaky (9.0 s).`) and a table with scenario, status (`flaky` for a flaky pass), time, attempts and the failure message (clipped to 300 characters; `|`, `<`, `>`, `&` and control characters neutralized). When the environment variable `GITHUB_STEP_SUMMARY` names a file, the same text is also appended to it, with or without the flag, so a plain `suite` step in a workflow adds its table to the job summary. The summary holds names, statuses, durations and failure messages only. `PATH` must not be a scenario, `--report` or `--junit` file (exit 2).

#### Output and exit codes

`PASS name (1.2 s)`, `FAIL name: first failing step - reason` (or the setup error), `SKIP name`, then `suite: 7/8 scenarios passed, FAILED: a, b` (`, SKIPPED: ...` when some were skipped). The JSON summary (`--report`, and the last line under `--json`) is `{"type":"suite_report","schema":1,"ocpp_version":"1.6","tool_version":"0.5.0","started_at":"...","status":"failed","counts":{"passed":7,"failed":1,"error":0,"skipped":0},"passed":false,"total":8,"passed_count":7,"failed":["a"],"failed_names":["a"],"error_names":[],"skipped":[],"scenarios":[{"name":"a","scenario":"a","status":"failed","ocpp_version":"1.6","source":"a.toml","station":"CP-001","started_at":"...","duration_ms":1200,"report":{...}}],"duration_ms":9000}`. `status` is `passed`, `failed`, `error` (never booted, or cut short; carries `error` instead of `report`) or `skipped`.

Exit codes: 0 only if every scenario passes, 1 if any does not, 2 for a usage error. On SIGINT and SIGTERM the running scenario is stopped, the rest are `skipped`, and the reports are written for what finished. The full table is in [configuration.md](configuration.md#kirby-ocpp-suite). More CI recipes: [ci.md](ci.md).

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
* With `--json` the aggregate is `{"type":"fleet_report","schema":1,"ocpp_version":"1.6","tool_version":"0.5.0","scenario":"charge-session","source":null,"started_at":"...","duration_ms":4200,"status":"passed","counts":{"passed":3,"failed":0,"error":0,"skipped":0},"passed":true,"total":3,"passed_count":3,"failed":[],"failed_names":[],"error_names":[],"stations":[{"station":"CP-1","schema":1,"ocpp_version":"1.6","tool_version":"0.5.0","scenario":"charge-session","source":null,"started_at":"...","duration_ms":4100,"status":"passed","passed":true}, ...]}`. A station that never booted has `"status": "error"`, `"passed": false` and an `"error"` text.
* `--timeout` covers the whole fleet, stagger included.
* `--report out.json` writes one file per station: `out.CP-1.json`. Characters of the id other than letters, digits, `-` and `_` become `_`, so ids that differ only in such characters would share a file.
* `--junit fleet.xml` writes one file for the whole fleet, with a suite named `scenario [CP-1]` per station.
* Exit code: 0 only if every station passes; see [configuration.md](configuration.md#kirby-ocpp-run). After an interruption no report files are written.

## CSMS scripts

`kirby-csms --script FILE` starts the server, runs the script against the stations that connect, prints the report and exits with its verdict. The stations are external processes (for example `kirby-ocpp`) that connect while the script waits. Steps run in order and the first failing step stops the run (later steps are `SKIP`).
- Top-level keys are `name` (required, not empty), an optional `[vars]` table (see [Variables, capture and payload checks](#script-variables-capture-and-payload-checks)) and `[[steps]]` (at least one); other top-level keys are rejected, except `description`, `ocpp_version`, `schema` and `tags`. Unknown fields inside a step are rejected, naming the step.

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
| `respond` | `call`, `payload`, optional `station`, `protocol` | Answer every inbound CALL of that action with this CALLRESULT payload; with `station` (and/or `protocol`) only that station's (that protocol's) CALLs, see [Response rules and fault injection](csms.md#response-rules-and-fault-injection) |
| `clear_respond` | `call`, optional `station`, `protocol` | Remove that responder (the one of the same scope) |
| `inject_fault` | `call`, `kind`, `payload`, `times` (default 1), optional `station`, `protocol`, `capture_id` | Make the next `times` CALLs of that action fail; see below. `capture_id = "name"` stores the id of the fault in the variable `name` |
| `remove_fault` | `id` | Remove one stored fault by id (`${var}` works); the step fails when no such fault is stored |
| `forget` | `station` | Forget a disconnected station; fails for an unknown or a connected station |
| `history` | `station`, optional `limit`, `expect_action` | Read the station's frame history; with `expect_action` the step fails unless the newest `limit` frames hold a CALL of that action |
| `rules` | optional `station`, `expect_actions` | Read the per-station rules (of `station`, or all); with `expect_actions` the step fails unless the sorted list of actions equals it |
| `faults` | optional `station`, `expect_count` | Read the stored faults (of `station`, or all); with `expect_count` the step fails unless that many faults are stored |

`wait_call` consumes the CALL it matches: each inbound CALL satisfies at most one `wait_call`, but waits may come in any order (a later wait can match a CALL that arrived before the previous wait's match).
- CALLs that arrive while other steps run are kept (the last 10 000, at most about 16 MiB); only CALLs some `wait_call` of the script names are remembered, and their payload only if a `wait_call` for that action uses `payload_contains` or `capture`.
- If the event stream lagged (events were lost), a `wait_call` that was already waiting when the loss happened and finds no match fails at once with a lost-events error instead of waiting out its timeout; waits that start later are not affected. CALLs a `wait_call` already took are dropped first when the memory limit is reached.

### Script variables, capture and payload checks

Scripts have the same variables as [emulator scenarios](#variables), with these differences: only the variables you define exist (no `${station_id}` or `${index}`: a script talks to many stations), and `capture` also feeds later steps.

- `[vars]` at the top level declares variables (strings, numbers, booleans). `kirby-csms --script FILE --var NAME=VALUE` (repeatable, needs `--script`, a bad name is a usage error, exit 2) overrides or adds one; `true`/`false`/numbers keep their type.
- `${name}` is replaced when the step runs, in `station`, `expect_status` and the JSON of `payload`, `payload_contains`, `expect_payload` and the payloads of `respond` and `inject_fault` (object keys are not substituted). A string that is exactly `"${name}"` keeps the variable's JSON type; `$${` is a literal `${`.
  - Action names (`call`, also in `wait_call`, `respond`, `clear_respond`, `inject_fault`) and typed fields (`ms`, `times`, `count`, `booted`, `timeout_ms`, `expect_error`) are not substituted. An unknown variable fails that step with a reason naming it.
  - One substitution may produce at most about 1 MiB, and CSMS values in failure reasons are cut to about 200 characters.
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

0 when the script passed, 1 for a failed or timed-out step, 2 for a usage or script error, 130 for SIGINT. The full table is in [configuration.md](configuration.md#kirby-csms---script).

## Reports

Both runners produce the same report structure, so tools can read either. Every JSON report carries version metadata and a `status` string, so a consumer can check the format before reading the rest.

### JSON (`--report`)

`--report PATH` writes pretty-printed JSON. Durations are in milliseconds, `index` is 0-based.

```json
{
  "schema": 1,
  "ocpp_version": "1.6",
  "tool_version": "0.5.0",
  "station": "CP-001",
  "source": "builtin:charge-session",
  "started_at": "2026-11-06T12:00:00.123Z",
  "scenario": "charge-session",
  "status": "failed",
  "passed": false,
  "cancelled": false,
  "steps": [
    { "index": 0, "name": "boot", "label": "boot", "action": "boot", "status": "passed", "duration_ms": 12 },
    {
      "index": 1, "name": "heartbeat", "label": "heartbeat",
      "action": "heartbeat", "status": "failed",
      "reason": "heartbeat: response {\"currentTime\":\"2026-11-06T12:00:00Z\"} does not contain {\"currentTime\":\"x\"}",
      "duration_ms": 31,
      "params": { "expect": { "payload": { "currentTime": "x" } } },
      "expected": { "currentTime": "x" },
      "actual": "{\"currentTime\":\"2026-11-06T12:00:00Z\"}"
    },
    { "index": 2, "name": "plug", "label": "plug c1", "action": "plug", "status": "skipped", "duration_ms": 0 }
  ],
  "duration_ms": 52
}
```

Report fields:

| Field | Level | Meaning |
| --- | --- | --- |
| `schema` | run, suite, fleet, station entry | Report format version, currently `1`. |
| `ocpp_version` | run, suite, suite scenario, fleet, station entry | Protocol of the run, currently `"1.6"`. |
| `tool_version` | run, suite, fleet, station entry | Version of the tool that wrote the report. |
| `station` | run, suite scenario, fleet entry | Station id. Absent for CSMS script reports. |
| `source` | run, suite scenario, fleet, station entry | Scenario file path or `builtin:NAME`; for a CSMS script, its file path. |
| `started_at` | run, suite, suite scenario, fleet, station entry | RFC 3339 UTC start time. |
| `status` | every level | `passed`, `failed`, `error` (the run produced no report) or `skipped`. The older booleans (`passed`, `cancelled`) stay. A step's `status` is `passed`, `failed` or `skipped`. |
| `counts` | suite, fleet | `{ "passed": n, "failed": n, "error": n, "skipped": n }`. |
| `label` | step | Stable name of the step: the explicit `name`, else the action plus its main parameters (`expect_call Heartbeat out`, `wait_for tx_active c1`, `start_tx c1 TAG`), with the iteration path appended in loops. |
| `action` | step | The step's action. |
| `params` | failed step | The step's resolved parameters. Secrets are redacted like frames: the value of a `ChangeConfiguration` for `AuthorizationKey` is `***`. The same applies to `expected`, `actual` and the failure reason, for `expect.payload`, `expect_payload` and `match` too, and to the JUnit failure text. |
| `expected`, `actual` | failed assertion | What an `expect`, `expect_call`, `expect_status` or `expect_payload` check wanted and what it saw (`actual` is clipped to about 200 characters). Absent when the runner has nothing to compare, for example a `wait_for` timeout has `expected` (the condition) only. |

`cancelled` is true when the run was stopped (timeout or signal). With `--json` the same object is printed on stdout as one line with an extra `"type": "report"`. Without `--json` the text form is:

```text
scenario smoke
  PASS  1 respond DataTransfer (0 ms)
  PASS  2 wait_station CP-1 (412 ms)
  FAIL  3 wait_call StartTransaction (20001 ms): no matching StartTransaction CALL within 20000 ms
  SKIP  4 heartbeat interval accepted (0 ms)
FAILED in 20413 ms
```

(`kirby-csms` prints `script <name>` as the first line.)

Every report level carries `started_at`, `duration_ms`, `status` and `ocpp_version`. The single run, each suite entry, the fleet report and each station entry also carry `scenario` and `source` (the suite report itself covers many scenarios, so it has neither), plus `station` where one applies. A suite entry keeps `name` as an alias of `scenario`. `ocpp_version` is taken from the run's own report. Entries that never started (`skipped`) have no `started_at`; an entry with `status` `error` has an `error` text instead of a `report`.

The suite report adds `type: "suite_report"`, `counts`, the name lists below and `scenarios` (one entry per scenario with `name`, `scenario`, `status`, `ocpp_version`, `source`, `station`, `started_at`, `duration_ms`, `attempts`, `flaky`, `attempt_results` and `report`; see [Retries](#retries-and-flaky-scenarios)). The suite report also carries `retries` and `flaky_names`. The fleet report adds `type: "fleet_report"`, `counts`, the name lists below and `stations` (one entry per station with `station`, `schema`, `ocpp_version`, `tool_version`, `scenario`, `source`, `started_at`, `duration_ms`, `status`, `passed`, and `error` when it never booted); its own `scenario`, `source`, `started_at` and `duration_ms` describe the whole run. For a station that never booted, `started_at` is the fleet start and `duration_ms` the fleet duration.

`counts` always matches the entries: `passed`, `failed` (scenarios or stations that ran and failed), `error` (never booted, or cut short) and `skipped` (not started). The name lists:

| Field | Contents |
| --- | --- |
| `failed` | Names or station ids with status `failed` or `error`, so `failed.length == counts.failed + counts.error`. |
| `failed_names` | Only status `failed`; its length is `counts.failed`. |
| `error_names` | Only status `error`; its length is `counts.error`. |
| `skipped` | Suite only: names with status `skipped`; its length is `counts.skipped`. |
| `flaky_names` | Suite only: names with status `passed` that failed at least once before (`--retries`); empty without retries. They are also in `counts.passed`. |

### JUnit XML (`--junit`)

One `<testsuite>` per run (a fleet run has one per station, named `scenario [CP-1]`), one `<testcase>` per step. A failed step has a `<failure>`, a skipped one `<skipped/>`. A run that never produced a report (the station could not connect or boot) is a suite with one `setup` test case holding an `<error>`.

Naming:

* `name` of a test case is the step's stable `label`, never its position. Names are unique within a suite: a repeated label gets ` #2`, ` #3` (a number already taken by an explicit label is skipped), and steps in loops already carry their ` [n]` suffix.
* The 1-based position is a `<property name="index">` of the test case, next to `action`.
* `classname` is `ocpp1.6.<suite name>`, so a fleet run gives `ocpp1.6.scenario [CP-1]`. It is a grouping label, not a structure to parse: names may contain `.` and `]`, and control characters are replaced by spaces.
* Each `<testsuite>` has `<properties>` `ocpp_version`, `station` (when known), `tool_version` and `source` (when known). A suite run with `--retries` above 0 adds `attempts` and, for a flaky pass, `flaky`; see [Retries](#retries-and-flaky-scenarios) for the `<flakyFailure>` and `<rerunFailure>` elements.
* The text of a `<failure>` holds the reason, then `action:`, `params:`, `expected:` and `actual:` lines where the report has them (params and expected are clipped).

```xml
<?xml version="1.0" encoding="UTF-8"?>
<testsuites tests="4" failures="1" errors="0" skipped="1" time="20.413">
  <testsuite name="smoke" tests="4" failures="1" errors="0" skipped="1" time="20.413">
    <properties>
      <property name="ocpp_version" value="1.6"/>
      <property name="tool_version" value="0.5.0"/>
      <property name="source" value="smoke.toml"/>
    </properties>
    <testcase classname="ocpp1.6.smoke" name="respond DataTransfer" time="0.000">
      <properties>
        <property name="index" value="1"/>
        <property name="action" value="respond"/>
      </properties>
    </testcase>
    <testcase classname="ocpp1.6.smoke" name="wait_call StartTransaction" time="20.001">
      <properties>
        <property name="index" value="3"/>
        <property name="action" value="wait_call"/>
      </properties>
      <failure message="no matching StartTransaction CALL within 20000 ms" type="StepFailed">no matching StartTransaction CALL within 20000 ms
action: wait_call
params: {"call":"StartTransaction","timeout_ms":20000}</failure>
    </testcase>
  </testsuite>
</testsuites>
```

Setup error:

```xml
<testcase classname="ocpp1.6.charge-session" name="setup" time="0.000">
  <error message="timed out waiting for the central system to accept the BootNotification" type="SetupError">...</error>
</testcase>
```

Text from a station (CALLERROR descriptions) can end up in step names and reasons; it is sanitized for the terminal and escaped for XML.

### Contracts and compatibility

These are public contracts and follow Semantic Versioning:

* The scenario and script file format. New optional keys and actions are minor changes. Removing or renaming a key, or changing what a valid file means, is a major change. The `schema` key names the format version; a build refuses a schema it does not know instead of guessing.
* The JSON report. Fields are added in minor releases and never renamed or removed within one `schema`. Consumers must ignore fields they do not know. A breaking change increments `schema`.
* The JUnit layout (suite and case names, properties) is part of the report contract. Test case names are stable labels so that CI history can follow a test across runs.
* Flag names. A flag is not renamed within a major version. `--ocpp-version`, `--protocol` and `--format` are reserved for future use; `suite` therefore selects GitHub output with the plain switch `--github-annotations`.

## Related pages

* [emulator.md](emulator.md): the emulator, fleet mode, response rules, TUI.
* [configuration.md](configuration.md): flags and environment variables of both binaries.
* [security.md](security.md): security profiles and certificates (for `request_certificate` and `security_event`).
* [architecture.md](architecture.md): scenario runner and script engine in the code.
