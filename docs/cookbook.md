# Cookbook

Short, copy-pasteable recipes for the most common jobs. Each one states the goal, the commands, what you should see and where the details live. Commands assume `kirby-ocpp` and `kirby-csms` are on your `PATH` ([install](../README.md#install)) and use `127.0.0.1` with port 9000. Recipes that use `suite` need version 0.5.0 or newer.

Contents: [First run](#first-run) · [Check a CSMS with the built-in scenarios](#check-a-csms-with-the-built-in-scenarios) · [Test a rejected tag](#test-a-rejected-tag) · [Lost connection and reconnect](#lost-connection-and-reconnect) · [Load test with a fleet](#load-test-with-a-fleet) · [Keep a CSMS setup in a configuration file](#keep-a-csms-setup-in-a-configuration-file) · [Script the central system](#script-the-central-system) · [TLS and mutual TLS](#tls-and-mutual-tls) · [Variables, captures and loops](#variables-captures-and-loops) · [Related pages](#related-pages)

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

## Answer a CSMS command with a scripted reply

Goal: see how a CSMS reacts when the station refuses a command or fails it with a CALLERROR. A `reply` step arms the answer; the next matching command gets it instead of the station's own handler.

```toml
# refused-reset.toml
name = "refused-reset"

[[steps]]
action = "reply"
call = "Reset"
result = { status = "Rejected" }
must_fire = true                    # fail when the CSMS never sent the Reset

[[steps]]
action = "expect_call"
call = "Reset"
direction = "in"
timeout_ms = 60000                  # time to send a Reset from the CSMS
```

```bash
kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id CP-1 --no-tui run --scenario refused-reset.toml
```

The scenario waits for a `Reset` from the central system, answers it `Rejected` and passes; the station does not restart. [reply](scenarios.md#reply) has `error`, `times` and `delay_ms`. The [conformance family](scenarios.md#conformance-family) uses the same step with a script that sends the commands.

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

## Keep a test setup in a configuration file

Goal: one file that describes the stations of a test setup, with a variant for a single station.

```toml
# lab.toml
[connection]
url = "ws://127.0.0.1:9000/ocpp"

[identity]
id = "LAB-{n:02}"

[fleet]
count = 3
stagger_ms = 50

[output]
no_tui = true

[profile.single.identity]
id = "LAB-01"

[profile.single.fleet]
count = 1
```

```bash
kirby-ocpp --config lab.toml run --scenario charge-session -q                  # three stations
kirby-ocpp --config lab.toml --profile single run --scenario charge-session -q # one station
kirby-ocpp --config lab.toml --count 2 run --scenario charge-session -q        # the flag wins over the file
kirby-ocpp --config lab.toml --profile single --print-config=toml              # what would run, as a file
```

Expected: `fleet: 3/3 stations PASSED`, `PASSED in N ms` and `fleet: 2/2 stations PASSED` for the runs against a local `kirby-csms --listen 127.0.0.1:9000`; the last command prints the merged configuration and exits 0 without connecting. Unknown keys, wrong types and a password in the URL exit 2 with the file and key named. Put the URL password in `OCPP_URL` instead of the file.

Reference: [Configuration file](configuration.md#configuration-file).

## Keep a CSMS setup in a configuration file

A server with a keyed station, a blocking rule and a fault, in one file. Secrets stay in separate files; the configuration only names them.

```bash
mkdir -p csms/secrets && cd csms
printf 'cp-001-key-0123456789\n' > secrets/cp-001.key && chmod 600 secrets/cp-001.key
cat > csms.toml <<'EOF'
[listener]
listen = "127.0.0.1:9000"

[security]
security_profile = 1

[output]
no_tui = true

[[station]]
id = "CP-001"
auth_key_file = "secrets/cp-001.key"

[[rule]]
action = "Authorize"
behavior = "respond"
payload = '{"idTagInfo":{"status":"Blocked"}}'

[[rule]]
action = "Heartbeat"
behavior = "call_error"
times = 2

[profile.fast.behavior]
heartbeat_interval = 5
EOF
kirby-csms --config csms.toml --profile fast --check-config   # OK: ... (profile fast): configuration is valid
kirby-csms --config csms.toml --profile fast --print-config=toml
kirby-csms --config csms.toml --profile fast
```

Expected: the first command prints one `OK:` line and exits 0 without opening the port; the second prints the merged file (no secrets, absolute paths); the third serves on `127.0.0.1:9000`. A station with the key from `secrets/cp-001.key` connects (`kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id CP-001`, key set through `ChangeConfiguration` of `AuthorizationKey`), its first two `Heartbeat` calls get a CALLERROR and every `Authorize` answers `Blocked`. An inline `default_auth_key = "..."` or `api_token = "..."` in the file exits 2 with the key and the `*_file` alternative named.

Reference: [kirby-csms configuration file](configuration.md#kirby-csms-configuration-file).

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

`validate` needs no connection and prints `ok loop-and-capture.toml (8 steps)`. The run reports flat step names: `data_transfer [1]`, `heartbeat [1]`, `data_transfer [2]`, and so on. `--var tag=ACME` replaces the `[vars]` value, so the tag becomes `ACME-1`. A `--var` for a name the scenario does not use is a usage error (exit 2). In a fleet each station gets its own `${index}` and `${station_id}`.

Reference: [Variables](scenarios.md#variables), [Loops](scenarios.md#loops).

## Debug a connection

Goal: find out why a charge point does not connect to a CSMS, and which side to fix.

```bash
kirby-ocpp check --url wss://csms.example/ocpp --id CP-1
```

Read the checklist from the top; the first `FAIL` line is the problem and the `hint:` under it names the flag or the setting to change:

* `dns` fails: the name does not resolve from this machine.
* `tcp` fails: nothing listens on the port, or a firewall drops the packets (`connection refused` against `timed out`).
* `tls` fails with `UnknownIssuer`: add the CA with `--tls-ca ca.pem`; with `NotValidForName`: set `--tls-server-name`. The line also shows the subject, the issuer and the expiry the server presented. For a self-signed test server `--tls-skip-verify` gets past the step, and the step warns when the certificate expires within 14 days.
* `websocket` fails with 401 or 403: the credentials (`user:password@` in the URL, the `AuthorizationKey` as the password) or the registration of the id are wrong. 404: the path is wrong, since the id is appended as the last path segment. A server that selects another subprotocol is named in the line; set `--ws-subprotocol`.
* `boot` shows `Rejected` (fails) or `Pending` (warns): the CSMS does not accept the station yet; register the id, vendor and model there.

For a script, `--json` prints one object and the exit code is 0 or 1:

```bash
kirby-ocpp check --url wss://csms.example/ocpp --id CP-1 --json | jq -r '.steps[] | select(.status == "failed") | "\(.name): \(.detail)"'
```

Reference: [Connection check](emulator.md#connection-check), [Configuration](configuration.md#kirby-ocpp-subcommands), [Connection errors](emulator.md#connection-errors).

## Look at or reset a station's state

Goal: see what a station remembers between runs, or start it from scratch.

```bash
kirby-ocpp state path --id CP-1          # where the file is
kirby-ocpp state show --id CP-1          # connectors, transactions, local list, configuration keys
kirby-ocpp state reset --id CP-1 --yes   # delete the state of CP-1 only
```

The secrets file next to the state file is never read or shown; `state show` only says that it exists. Reference: [Configuration](configuration.md#kirby-ocpp-subcommands).

## Related pages

[Run kirby in CI](ci.md) · [Scenarios](scenarios.md) · [Emulator](emulator.md) · [CSMS](csms.md) · [Configuration](configuration.md) · [Docs index](README.md)
