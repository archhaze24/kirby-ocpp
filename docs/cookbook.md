# Cookbook

Short, copy-pasteable recipes for the most common jobs. Each one states the goal, the commands, what you should see and where the details live. Commands assume `kirby-ocpp` and `kirby-csms` are on your `PATH` ([install](../README.md#install)) and use `127.0.0.1` with port 9000. Recipes that use `suite` need version 0.5.0 or newer.

Contents: [First run](#first-run) · [Check a CSMS with the built-in scenarios](#check-a-csms-with-the-built-in-scenarios) · [Test a rejected tag](#test-a-rejected-tag) · [Lost connection and reconnect](#lost-connection-and-reconnect) · [Load test with a fleet](#load-test-with-a-fleet) · [Script the central system](#script-the-central-system) · [TLS and mutual TLS](#tls-and-mutual-tls) · [Variables, captures and loops](#variables-captures-and-loops) · [Related pages](#related-pages)

## First run

Goal: see a charge point talk to a central system on your machine.

```bash
# terminal 1: the central system
kirby-csms --listen 127.0.0.1:9000

# terminal 2: a charge point
kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id CP-1
```

Both open a terminal UI. The CSMS lists `CP-1` as connected and the emulator log shows the BootNotification exchange. Press `?` in either UI for the key help; `:` opens the action palette in the emulator. Add `--no-tui` to either command for plain log lines.

Reference: [Emulator](emulator.md#quick-start), [CSMS](csms.md#quick-start).

## Check a CSMS with the built-in scenarios

Goal: a pass/fail verdict and a JUnit file for your CSMS, with no scenario files to write.

```bash
kirby-ocpp --url ws://csms.example.test/ocpp --id CP-1 \
  suite --all-builtins --junit junit.xml --report suite.json
echo $?
```

Each built-in runs on a fresh station, one after another. The output is one line per scenario and a summary such as `suite: 8/8 scenarios passed`. The exit code is 0 only if every scenario passed, so the command can gate a CI job. `junit.xml` has one test suite per scenario; `suite.json` has the machine-readable summary. `kirby-ocpp scenarios list` names the built-ins.

Reference: [Suite runs](scenarios.md#suite-runs), [Built-in scenarios](scenarios.md#built-in-scenarios), [Run kirby in CI](ci.md).

## Test a rejected tag

Goal: prove that a station does not start a session for a blocked tag.

The scenario asserts the status the CSMS answered. `authorize` and `start_tx` expect `Accepted` unless you set another status:

```toml
# blocked-tag.toml
name = "blocked-tag"

[[steps]]
action = "plug"
connector = 1

[[steps]]
action = "authorize"
id_tag = "BAD-TAG"
expect = { status = "Blocked" }

[[steps]]
action = "start_tx"
connector = 1
id_tag = "BAD-TAG"
expect = { status = "Blocked" }
```

Against your own CSMS, configure it to block `BAD-TAG` and run:

```bash
kirby-ocpp --url ws://127.0.0.1:9000/ocpp run --scenario blocked-tag.toml
```

To try it against `kirby-csms`, make it answer `Blocked` with a script (it exits when the script ends, so the last step waits for the session attempt):

```toml
# block-every-tag.toml
name = "block-every-tag"

[[steps]]
action = "respond"
call = "Authorize"
payload = { idTagInfo = { status = "Blocked" } }

[[steps]]
action = "respond"
call = "StartTransaction"
payload = { transactionId = 0, idTagInfo = { status = "Blocked" } }

[[steps]]
action = "wait_call"
call = "StartTransaction"
timeout_ms = 30000
```

```bash
kirby-csms --listen 127.0.0.1:9000 --script block-every-tag.toml &
kirby-ocpp --url ws://127.0.0.1:9000/ocpp run --scenario blocked-tag.toml
wait
```

Expected: three `PASS` lines and `PASSED`, exit 0. Against a CSMS that accepts the tag, the run stops at `authorize: expected status Blocked, got Accepted` with exit 1 and the later steps are `SKIP`. With a long-running `kirby-csms` the same effect comes from the HTTP API: `curl -X PUT 127.0.0.1:9100/v1/rules/Authorize -d '{"idTagInfo":{"status":"Blocked"}}'` (start it with `--api-listen 127.0.0.1:9100`).

Reference: [Expect](scenarios.md#expect), [CSMS scripts](scenarios.md#csms-scripts), [HTTP API](api.md#rules-and-faults).

## Lost connection and reconnect

Goal: check what your CSMS does when a station drops, and that the station comes back.

Run the built-in `reconnect` scenario: it disconnects, waits until the link is down, connects again, waits for an accepted registration and sends a Heartbeat.

```bash
kirby-ocpp --url ws://127.0.0.1:9000/ocpp run --scenario reconnect
```

To drop the link from the CSMS side, give the station a rule that closes the WebSocket on the first `Reset`, and shorten the backoff so the retry is quick:

```bash
kirby-csms --listen 127.0.0.1:9000 --api-listen 127.0.0.1:9100 --no-tui &
kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id CP-1 --no-tui \
  --rule 'Reset=drop*1' --reconnect-initial-ms 200 --reconnect-max-ms 2000 &
curl -s -X POST 127.0.0.1:9100/v1/stations/CP-1/calls -d '{"action":"Reset","payload":{"type":"Soft"}}'
```

The call fails with `station_not_connected` (the connection closed before a reply). The emulator log shows `response rule: Reset -> drop` and `reconnect attempt 1 in 200ms`, and `curl 127.0.0.1:9100/v1/stations` lists `CP-1` as connected again.

To test the opposite case, add `--no-reconnect` (or `--reconnect-max-attempts N`). When the connection is lost for good a headless station or `run` prints `reconnect stopped (...)` with the last error and exits 1.

Reference: [Response rules](emulator.md#response-rules), [Connection errors](emulator.md#connection-errors), [Flags](configuration.md#kirby-ocpp).

## Load test with a fleet

Goal: many stations against one CSMS.

```bash
kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id-template 'LOAD-{n:03}' --count 200 \
  --stagger-ms 20 --no-tui --log-file fleet.log
```

`--count` takes 1 to 1000. `{n:03}` is the station index padded to 3 digits (`LOAD-001` to `LOAD-200`) and the template needs a placeholder when `--count` is above 1. `--stagger-ms` waits between starting two stations so the CSMS is not hit by 200 handshakes at once. Every log line is tagged with the station id.

To run a scenario on every station at the same time and get one verdict:

```bash
kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id 'LOAD-{n:03}' --count 20 --stagger-ms 50 \
  run --scenario charge-session --junit fleet.xml -q
```

Expected: `fleet: 20/20 stations PASSED`, exit 0; otherwise `fleet: 19/20 stations passed, FAILED: LOAD-007` and exit 1. `--timeout` covers the whole fleet, stagger included. Each station keeps its own state file named after its id; add `--no-persist` for throwaway runs.

Reference: [Fleet mode](emulator.md#fleet-mode), [Fleet runs](scenarios.md#fleet-runs), [Id templates](configuration.md#id-templates).

## Script the central system

Goal: drive `kirby-csms` from CI to test charge point firmware, either with a script file or over HTTP.

A script waits for a station, sends commands and checks the answers:

```toml
# csms-smoke.toml
name = "csms-smoke"

[[steps]]
action = "wait_station"
station = "CP-1"
booted = true
timeout_ms = 20000

[[steps]]
name = "set heartbeat interval"
action = "call"
station = "CP-1"
call = "ChangeConfiguration"
payload = { key = "HeartbeatInterval", value = "60" }
expect_status = "Accepted"

[[steps]]
name = "the value was stored"
action = "call"
station = "CP-1"
call = "GetConfiguration"
payload = { key = ["HeartbeatInterval"] }
expect_payload = { configurationKey = [{ key = "HeartbeatInterval", value = "60" }] }
```

```bash
kirby-csms --listen 127.0.0.1:9000 --script csms-smoke.toml --junit csms-junit.xml --report csms-report.json --timeout 60 &
# start your firmware (or kirby-ocpp --id CP-1 --no-tui) pointing at ws://127.0.0.1:9000/ocpp
wait $!
echo $?
```

The server exits when the script ends: 0 if every step passed, 1 otherwise.

For a long-running server, turn on the HTTP API and use `curl`:

```bash
kirby-csms --listen 127.0.0.1:9000 --api-listen 127.0.0.1:9100 --no-tui &

curl -s 127.0.0.1:9100/v1/stations
curl -s -X POST 127.0.0.1:9100/v1/stations/CP-1/calls \
  -d '{"action":"ChangeConfiguration","payload":{"key":"HeartbeatInterval","value":"60"}}'
curl -sN '127.0.0.1:9100/v1/events?station=CP-1'
```

The first call lists the stations (`id`, `connected`, `connectors`, ...). The second answers `{"ok":true,"outcome":"result","result":{"status":"Accepted"}}`. The last one streams server-sent events (`connected`, `frame`, `state`, ...) until you stop it. A non-loopback `--api-listen` address needs a token.

Reference: [CSMS scripts](scenarios.md#csms-scripts), [HTTP API](api.md).

## TLS and mutual TLS

Goal: connect a station to a CSMS over `wss://`, with and without a client certificate.

Create a test CA, a server certificate for `localhost` and a client certificate with the common name `CP-1` using the `openssl` commands in [Security](security.md#tls-and-mtls-how-to). Then:

```bash
# server authentication only
kirby-csms --listen 127.0.0.1:9000 --tls-cert server.pem --tls-key server.key
kirby-ocpp --url wss://localhost:9000/ocpp --id CP-1 --tls-ca ca.pem

# mutual TLS (security profile 3): the certificate's CN must equal the station id
kirby-csms --listen 127.0.0.1:9000 --security-profile 3 \
  --tls-cert server.pem --tls-key server.key --tls-client-ca ca.pem
kirby-ocpp --url wss://localhost:9000/ocpp --id CP-1 --tls-ca ca.pem \
  --tls-cert cp1.pem --tls-key cp1.key
```

With mutual TLS the station connects and the CSMS prints `connected ... (ocpp1.6, profile 3)`. Without `--tls-cert` and `--tls-key` the handshake fails (`received fatal alert: CertificateRequired`). Basic Auth profiles 1 and 2 are covered on the same page.

Reference: [Security](security.md), [TLS flags](configuration.md#kirby-ocpp).

## Variables, captures and loops

Goal: one scenario that repeats a step, reuses a value from an earlier answer and takes a value from the command line.

```toml
# loop-and-capture.toml
name = "loop-and-capture"

[vars]
vendor = "com.example"
tag = "KIRBY-TAG"

[[steps]]
action = "data_transfer"
vendor_id = "${vendor}"
message_id = "hello-${station_id}"
capture = { first_status = "/status" }

[[steps]]
action = "repeat"
count = 3
name = "round"

  [[steps.steps]]
  action = "data_transfer"
  vendor_id = "${vendor}"
  message_id = "round-${i}"
  data = "previous status was ${first_status}"

  [[steps.steps]]
  action = "heartbeat"

[[steps]]
action = "authorize"
id_tag = "${tag}-${index}"
```

```bash
kirby-ocpp scenarios validate loop-and-capture.toml
kirby-ocpp --url ws://127.0.0.1:9000/ocpp run --scenario loop-and-capture.toml --var tag=ACME
```

`validate` needs no connection and prints `ok loop-and-capture.toml (7 steps)`. The run reports flat step names: `data_transfer [1]`, `heartbeat [1]`, `data_transfer [2]`, and so on. `--var tag=ACME` replaces the `[vars]` value, so the tag becomes `ACME-1`. A `--var` for a name the scenario does not use is a usage error (exit 2). In a fleet each station gets its own `${index}` and `${station_id}`.

Reference: [Variables](scenarios.md#variables), [Loops](scenarios.md#loops).

## Related pages

[Run kirby in CI](ci.md) · [Scenarios](scenarios.md) · [Emulator](emulator.md) · [CSMS](csms.md) · [Configuration](configuration.md) · [Docs index](README.md)
