# Run kirby in CI

Use `kirby-ocpp` and `kirby-csms` as test runners: the exit code gates the job, JUnit XML feeds the test report, and the JSON report keeps the evidence. This page has ready workflows for GitHub Actions and GitLab CI, the exit-code and timeout rules, and a guide to reading failures.

Contents: [Overview](#overview) · [GitHub Actions](#github-actions) · [Conformance job](#conformance-job) · [GitLab CI](#gitlab-ci) · [Exit codes and timeouts](#exit-codes-and-timeouts) · [Reports](#reports) · [Read a failure](#read-a-failure) · [Avoid flaky runs](#avoid-flaky-runs) · [Related pages](#related-pages)

## Overview

Two directions, both covered by the same commands:

* Test a central system: run `kirby-ocpp suite` (or `run`) against it. The steps below use `kirby-csms` as the system under test so the workflow runs as is; replace the "start the CSMS" step with the start of your own and point `--url` at it.
* Test charge point firmware: run `kirby-csms --script` and let the firmware connect. See [Script the central system](cookbook.md#script-the-central-system).

`suite` needs version 0.5.0 or newer. Install with `npm install -g kirby-ocpp kirby-csms`, `cargo install kirby-ocpp kirby-csms` or a release archive ([all channels](distribution.md)).

## GitHub Actions

```yaml
name: ocpp

on: [push, pull_request]

permissions:
  contents: read
  checks: write          # lets the report action create a check run

jobs:
  scenarios:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v7

      - uses: actions/setup-node@v7
        with:
          node-version: 22

      - name: Install kirby
        run: npm install -g kirby-ocpp kirby-csms

      - name: Start the central system
        run: |
          kirby-csms --listen 127.0.0.1:9000 --no-tui --log-file csms.jsonl > csms.log 2>&1 &
          echo $! > csms.pid
          for _ in $(seq 50); do
            grep -q '^listening on' csms.log && exit 0
            sleep 0.2
          done
          cat csms.log
          exit 1

      - name: Run the scenarios
        run: |
          kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id CP-1 \
            suite --all-builtins \
            --timeout 30 --retries 1 --jobs 4 --github-annotations \
            --junit junit.xml --report suite.json

      - name: Stop the central system
        if: always()
        run: kill "$(cat csms.pid)" || true

      - name: Publish the test report
        if: always()
        uses: mikepenz/action-junit-report@v6
        with:
          report_paths: junit.xml

      - name: Upload reports
        if: always()
        uses: actions/upload-artifact@v7
        with:
          name: ocpp-reports
          path: |
            junit.xml
            suite.json
            csms.log
            csms.jsonl
```

Notes:

* The CSMS prints `listening on ws://127.0.0.1:9000` once it accepts connections; the loop waits for that line instead of sleeping. With `--listen 127.0.0.1:0` the line shows the real port.
* `suite --all-builtins` runs every built-in except `finish-session`. Add your own scenarios by naming files or a directory (`suite --all-builtins scenarios/`, every `*.toml` directly inside). A step that fails fails the job; the `if: always()` steps still publish what exists.
* `--github-annotations` prints an `::error` line per failed scenario (shown on the run page and, with `file=`, on the file), `--retries 1` runs a failed scenario once more on a fresh station and reports a pass after a failure as flaky (exit code 0), and `--jobs 4` runs four scenarios at a time (each station gets its own id: `CP-1`, `CP-1-2`, `CP-1-3`, `CP-1-4`). Inside GitHub Actions the suite also appends a Markdown table to the job summary (`$GITHUB_STEP_SUMMARY`); `--summary-md PATH` writes the same table to a file. See [Suite runs](scenarios.md#suite-runs) for the exact rules.
* The two action steps use `mikepenz/action-junit-report`; any reporter that reads JUnit XML works, for example `dorny/test-reporter` with `reporter: java-junit`. Action names and versions here were current when this page was written; pin the ones you use.
* A CSMS you test on a network host needs no `kirby-csms` steps: put its URL in `--url` and add `--id`, `--tls-*` or Basic Auth credentials in the URL as needed ([Security](security.md)).

### Select scenarios by tag

Give scenario files a top-level `tags` list and pick them per job:

```toml
name = "quick-login"
tags = ["smoke"]

[[steps]]
action = "heartbeat"
```

```bash
# pull requests: the smoke set only; nightly: everything except the slow ones
kirby-ocpp --url ws://127.0.0.1:9000/ocpp suite scenarios/ --tag smoke
kirby-ocpp --url ws://127.0.0.1:9000/ocpp suite scenarios/ --skip-tag slow --skip 'wip-*'
```

`--filter` and `--skip` take globs over the scenario name, `--tag` needs every given tag, `--skip-tag` drops on any. A selection that matches nothing exits 2, so a typo in a tag fails the job instead of passing with no tests.

### Conformance job

The scenarios of the [conformance family](scenarios.md#conformance-family) run against `kirby-csms` and its script, which sends the CSMS commands. A job in a checkout of the repository:

```yaml
  conformance:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v7

      - uses: actions/setup-node@v7
        with:
          node-version: 22

      - name: Install kirby
        run: npm install -g kirby-ocpp kirby-csms

      - name: Run the family
        run: |
          kirby-csms --listen 127.0.0.1:9000 --no-tui --script conformance/csms-script.toml \
            --timeout 180 --junit csms-junit.xml --report csms-report.json > csms.log 2>&1 &
          CSMS=$!
          for _ in $(seq 50); do
            grep -q '^listening on' csms.log && break
            sleep 0.2
          done
          kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id CP-CONF --no-tui \
            suite conformance/scenarios --tag conformance \
            --junit junit.xml --report suite.json --github-annotations
          status=$?
          wait $CSMS || status=1
          exit $status

      - uses: actions/upload-artifact@v7
        if: always()
        with:
          name: conformance
          path: |
            junit.xml
            suite.json
            csms-junit.xml
            csms-report.json
```

The emulator and the script each exit non-zero when something failed, so the step needs both exit codes. Run the whole family in one go with `--jobs 1` (the default) and no `--retries`; the script waits for the scenarios in file order. A failure shows up in `junit.xml` (scenario and step) or in `csms-junit.xml` (the answer the station gave).

## GitLab CI

```yaml
ocpp:
  image: node:22
  timeout: 15m
  before_script:
    - npm install -g kirby-ocpp kirby-csms
  script:
    - kirby-csms --listen 127.0.0.1:9000 --no-tui > csms.log 2>&1 &
    - |
      for i in $(seq 50); do
        grep -q '^listening on' csms.log && break
        sleep 0.2
      done
      grep -q '^listening on' csms.log
    - kirby-ocpp --url ws://127.0.0.1:9000/ocpp suite --all-builtins --timeout 30 --junit junit.xml --report suite.json
  artifacts:
    when: always
    paths: [junit.xml, suite.json, csms.log]
    reports:
      junit: junit.xml
```

GitLab shows the JUnit report on the pipeline's Tests tab and in merge requests. The shell is `sh`, so the snippet uses only POSIX syntax.

## Exit codes and timeouts

| Code | Meaning in CI |
| --- | --- |
| 0 | Everything passed |
| 1 | A test failed, timed out or could not connect, or a report file could not be written |
| 2 | The command is wrong (flag, scenario file): fix the pipeline, not the CSMS |
| 130 | Interrupted by SIGINT |

The full per-command table is in [configuration.md](configuration.md#exit-codes). Run commands directly as a step so a non-zero code fails the job. Do not append `|| true`; if you must clean up after the run, use `if: always()` (GitHub) or `after_script` (GitLab).

Timeouts, from the inside out:

* `--call-timeout-ms` (default 30000) is how long the station waits for one answer from the CSMS.
* `wait_for` and `expect_call` steps have their own `timeout_ms` (default 5000).
* `--timeout SECONDS` limits a whole scenario, connecting included (`run`, and per scenario in `suite`; for a fleet run it covers the whole fleet). When it expires the running step fails with `cancelled` and the rest are skipped. For `kirby-csms --script` it limits the whole script.
* A CSMS that is down costs the full `--timeout` for every scenario of a `suite`: the station keeps retrying the connection until the timeout ends the scenario. For CI pass `--no-reconnect` and `--fail-fast`: a refused connection then fails the scenario at once (`reconnect stopped (automatic reconnect is disabled)`), and `--fail-fast` reports the remaining scenarios as `SKIP`. `--no-reconnect` also applies during a scenario: an unexpected disconnect fails the run instead of being retried.
* The job timeout (`timeout-minutes`, `timeout:`) is the last guard. A run stopped by SIGINT or SIGTERM from the runner still writes the `suite` report; see the signal rules in [configuration.md](configuration.md#kirby-ocpp-suite).

## Reports

| File | Option | Use |
| --- | --- | --- |
| JUnit XML | `--junit PATH` | Feed it to the CI test report. One test case per step with a stable name; a suite has one `<testsuite>` per scenario |
| JSON report | `--report PATH` | Archive it and read it with `jq`; it carries the failing step's expected and actual values |
| CSMS event log | `--log-file PATH` | Every frame as JSON lines: what the CSMS under test saw and sent |

For a fleet run `--report out.json` writes one file per station (`out.CP-1.json`). Formats and fields: [Reports](scenarios.md#reports). Writing the files does not need a TTY; `-q` keeps the console to the verdict lines and `-v` adds every frame.

## Read a failure

Start with the console: a failed scenario prints `FAIL name: first failing step - reason`, then the suite summary names every failed scenario. For the evidence use the JSON report. The fields below are the ones to read (the full list is in the [report fields table](scenarios.md#json---report)):

* `status` of the run, scenario or step: `passed`, `failed`, `error` (the station never booted, or the run was cut short; the entry has an `error` text instead of `report`) or `skipped`.
* `steps[].reason`: the failure message, for example `authorize: expected status Blocked, got Accepted`.
* `steps[].label`, `action` and `index`: which step; the label is stable across runs.
* `steps[].params`: the step's resolved parameters. Secrets are redacted.
* `steps[].expected` and `steps[].actual`: what the assertion wanted and what it saw (`actual` is clipped to about 200 characters).

Print every failed step of a suite report:

```bash
jq -r '.scenarios[] | select(.status != "passed") | .name as $n
  | (.report.steps // [])[] | select(.status == "failed")
  | "\($n): step \(.index) \(.label): \(.reason)"' suite.json
```

For a scenario with `status` `error`, print its `error` text instead (`jq -r '.scenarios[] | select(.status=="error") | "\(.name): \(.error)"' suite.json`). The same data is in the JUnit file: a failed case has a `<failure>` with the reason, `action:`, `params:`, `expected:` and `actual:` lines, and a station that never booted is one errored `setup` case. For the wire-level view, read the frames in the `--log-file` of the CSMS or run the emulator with `-v`.

## Avoid flaky runs

* Wait for readiness, not for time: the `listening on` line for `kirby-csms`, an accepted BootNotification for the station (`run` and `suite` do this for you).
* Set `--timeout` per scenario high enough for a cold CI machine (for example 30 to 60 seconds). Raise `timeout_ms` of single `wait_for` and `expect_call` steps rather than adding `sleep` steps.
* Give every scenario a fresh station: `suite` does, and `run` is ephemeral by default (no state file). Do not combine `--persist` with CI runs.
* Use a fixed station id with `--id` when the CSMS under test has an allow-list. With `--jobs N` the allow-list also needs `ID-2` to `ID-N`.
* `--retries` hides nothing: a scenario that passed on a retry is listed under `FLAKY:` in the summary line, in `flaky_names` of the report, as a `::warning` with `--github-annotations` and as `flaky` in the JUnit file. Track that list and fix the scenario rather than raising the retry count.
* Use `-q` in logs you read often and `-v` when diagnosing; keep `--log-file` on the CSMS side as an artifact.
* Fix the ports. Use `127.0.0.1` rather than `localhost`, which can resolve to IPv6 first.
* Start the CSMS per job, not shared between jobs, so state and transaction ids do not leak across runs.

## Related pages

[Cookbook](cookbook.md) · [Scenarios](scenarios.md) · [Configuration](configuration.md) · [Distribution](distribution.md) · [Docs index](README.md)
