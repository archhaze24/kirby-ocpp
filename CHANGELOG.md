# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). The release workflow publishes the section of the
tag as the GitHub release notes.

## [Unreleased]

### Fixed

- CSMS startup installs configured rules and event subscriptions before accepting stations. Replaced sessions keep their own protocol when matching rules and faults.
- Scenario `expect_call` keeps collecting events during other steps, so long waits do not cause false event-loss failures.
- Both tools redact `AuthorizationKey` in malformed inbound frame logs, including escaped property names. Invalid JSON is omitted unless `--show-secrets` is enabled.

- The emulator percent-encodes the station id and the `AuthorizationKey` taken from the configuration registry before they reach the connection URL, so `%` sequences in either reach the wire literally. A password given in the URL is still percent-decoded once.
- `kirby-csms` accepts, for OCPP 1.6 sessions, a Basic Auth password that holds the decoded bytes of a hexadecimal `AuthorizationKey` as OCPP-J 1.6 section 6.2.2 specifies, besides the literal key. Sessions on other subprotocols accept only the literal key.

- The emulator no longer reconnects or queues a `ReconfigurationOfSecurityParameters` event for writes that cannot change the connection: `SecurityProfile` set to the value it already has (0 or 1), and `AuthorizationKey` written while profile 0 or 3 is in force.
- Disconnects, hard resets and security reconnects publish the frames the connection had already received instead of discarding them.
- The `kirby-csms` terminal UI shows every connector row in stacked layouts (for example 110x30) when the transaction table has room to spare.

- `kirby-csms --record` writes the negotiated subprotocol of each session into its frames (`protocol`, absent when none was negotiated). `--replay` refuses a recording that names a protocol other than `ocpp1.6`; recordings without the key still replay as 1.6.

### Added

- `--authorization-key-encoding literal|hex` (`OCPP_AUTHORIZATION_KEY_ENCODING`, `[tls].authorization_key_encoding`) selects how the emulator turns the 1.6 `AuthorizationKey` into the Basic Auth password. `literal` (default) sends the key text; `hex` sends the decoded bytes and accepts only hexadecimal keys.
- `kirby-csms --subprotocol require:NAME,NAME` accepts a list: the first subprotocol the client offers (in the client's order) that is on the list is selected, and a client that offers none of them is refused with HTTP 400. `kirby-csms-core` has the matching `SubprotocolPolicy::RequireAny`.

## [0.6.0] - 2026-11-14

**The automation release: configuration files and profiles for both tools, a connection doctor, session recording and replay, scripted replies and a conformance family, a much larger HTTP control API with TLS, OpenAPI and a `ctl` client, and per-station rules and faults in `kirby-csms`.**

### 0.6.0 in numbers

| Metric | Since 0.5.0 |
| --- | --- |
| 🧾 Commits | **55** in eight days (7 to 14 November 2026) |
| ✏️ Lines of code | **+37.6k / −2.6k** across 195 files; Rust grew from 86.9k to **118.7k** lines |
| 🧪 Tests | **+402** new test functions: 1,330 → **1,732** (all run in the `nextest` gate) |
| 📦 Crates | 7 → **7** (new dependency: `clap_mangen`) |
| 🔎 Reviews | Whole-branch correctness, security, documentation and product reviews that ran the real binaries and both terminal UIs |
| ✨ New features | **35** |
| 🔧 Changes | **9** |
| 🐛 Fixes and hardening | **10** |

### ✨ Added

- `kirby-csms --record` writes frames from all stations to one redacted JSONL recording.
- `kirby-csms --replay FILE` sends the CALLs a CSMS recording holds to the connecting stations, in recorded order with fresh message ids and the recorded pauses, and exits with a verdict (0 passed, 1 failed, 2 invalid recording, 130 SIGINT). A `RemoteStopTransaction` is tied to the live `StartTransaction` of the same station, connector and `idTag`; a recorded transaction id is never sent, and the run fails when the live start differs, is ambiguous or missing. The file is validated before the server binds: a redacted CALL, several sessions of a station or a transaction id outside `RemoteStopTransaction` are refused. `--timeout` now also applies to a replay. See [CSMS replay](docs/csms.md#replay).
- `kirby-csms-core`: `CsmsServer::start_with` and `StartOptions` attach a recorder and an opt-in bounded `Observer` of session and transaction facts before connections are accepted; `recording::holds_redacted` is public. A recording reader now refuses a second `end` line and entries after it.
- OpenAPI document `kirby-csms-openapi.json` for the control API, included in each release archive and as a standalone release asset. A test checks route and method coverage, required path parameters and local schema references.
- `POST /v1/calls` sends a CALL to up to 64 stations selected by a station-id glob and optional protocol. Admission reserves the shared command and payload budgets before sending; the response reports each station's outcome in a bounded, stable shape.
- CSMS configuration `[[command]]` blocks run calls after a delay, at fixed-delay intervals or once per observed live connection. Connection hooks are tied to the original session; reconnects cannot receive a stale hook. Commands share the API and stdin limits, reject inline `AuthorizationKey` values and stop before station shutdown.
- Manual pages for `kirby-ocpp`, its subcommands (`kirby-ocpp-run(1)`, `kirby-ocpp-suite(1)`, `kirby-ocpp-state-show(1)`, ...) and `kirby-csms`, generated from the `--help` definitions. They ship in `man/man1/` of every release archive and are installed by the Homebrew formula. A test fails when a flag is missing from a page. See [Release archives](docs/distribution.md#release-archives).
- `kirby-ocpp --config FILE` (`KIRBY_OCPP_CONFIG`) reads settings from a TOML file and `--profile NAME` (`KIRBY_OCPP_PROFILE`) applies a `[profile.NAME]` variant. Every flag except the per-invocation ones has a key (the long flag name with `_`), grouped in tables that match the `--help` headings. Precedence: flag, environment variable, profile, file, default. Unknown keys and wrong types are errors that name the file and the key path (exit 2); relative paths resolve against the directory of the file; a password never goes into the file. See [Configuration file](docs/configuration.md#configuration-file).
- `--print-config=toml` prints the effective configuration as a file that `--config` accepts (the same text comes back when it is printed again); secrets are never printed. `--print-config` alone still prints JSON.
- `kirby-csms --config FILE` (`KIRBY_CSMS_CONFIG`), `--profile NAME` (`KIRBY_CSMS_PROFILE`), `--print-config[=json|toml]` and `--check-config`: the same file format, precedence and error style as the emulator, with tables named after the `--help` groups (`[listener]`, `[api]`, `[tls]`, `[security]`, `[behavior]`, `[output]`). `[[station]]` blocks (`id`, `auth_key_file`) and `[[rule]]` blocks (response rules and faults for all stations, `action`, `behavior`, `payload`, `times`) take effect at start; every block accepts `protocol = "ocpp1.6"`. The file never holds a secret: `api_token`, `auth_key` and `default_auth_key` are refused with the `*_file` alternative named. New `--default-auth-key-file` (`KIRBY_CSMS_DEFAULT_AUTH_KEY_FILE`) reads the default Basic Auth key from a file. `--check-config` validates the file, the profile, the flags, the key files and certificates, prints one `OK:` line and never binds a port. `kirby-csms --help` now groups the flags under headings. See [kirby-csms configuration file](docs/configuration.md#kirby-csms-configuration-file).
- `kirby-ocpp suite` selection: `--filter GLOB` and `--skip GLOB` (globs over the scenario name, repeatable), `--tag TAG` and `--skip-tag TAG` (the top-level `tags` of a scenario file are now effective; repeated `--tag` needs every tag). Applied in that order; a selection that leaves no scenario exits 2.
- `suite --retries N` (0 to 10) runs a failed scenario again on a fresh station. `status` stays the status of the last attempt; a pass after a failure is flaky: it exits 0 and shows as `FLAKY:` in the summary line, `flaky_names`, `attempts`, `flaky` and `attempt_results` in the JSON report, the properties `attempts` and `flaky` plus `<flakyFailure>` / `<flakyError>` (`<rerunFailure>` / `<rerunError>` for a final failure) in the JUnit file, with test case names unchanged.
- `suite --jobs N` (1 to 16) runs scenarios in parallel; slot 2 and up append `-2`, `-3`, ... to the station id. Reports keep the order of the selection.
- `kirby-ocpp check`: a connection doctor that tests the way to a Central System one step at a time (`url`, `dns`, `tcp`, `tls`, `websocket`, `boot`, `close`) with the connection flags of a station, prints `PASS`, `WARN`, `FAIL` or `SKIP` per step with a hint for the failures, and exits 1 when a step fails. It reports the server certificate (subject, issuer, expiry, how it was verified; a certificate that expires within 14 days warns), the selected subprotocol, the HTTP status of a refused upgrade, and the BootNotification status, interval and clock difference. `--timeout SECONDS` limits the whole check; `--json` prints one object (`schema`, `ocpp_version`, `tool_version`, `url`, `status`, `steps`). It sends one BootNotification and never touches the station state. See [Connection check](docs/emulator.md#connection-check).
- `kirby-ocpp state path|show|reset`: print the state directory or the file of a station (`path`), summarize a state file without printing secrets (`show`, also `--json`) and delete the state of one station (`reset`, with a prompt on a terminal or `--yes`). A file of a newer version is refused untouched.
- `kirby-csms` rules and faults per station and protocol. A response rule or fault can be limited to one station id and/or one protocol; a station's own fault or rule wins over a global one (faults, then schema validation, then station rule, global rule with a protocol, global rule, default answer). HTTP API (additive under `/v1`): `GET /v1/stations/{id}/rules`, `GET`, `PUT` (optional `?protocol=`) and `DELETE /v1/stations/{id}/rules/{action}`, `station` and `protocol` fields on `POST /v1/faults`, fault ids in `GET /v1/faults` (new `faults` list next to `remaining`) and `DELETE /v1/faults/{id}`; the new error code `unknown_rule`. The terminal UI forms for rules and faults have an optional station field and list station rules as `Action@station`. Caps: 1000 faults (100 per station), 1000 station rules (64 per station), 8 MiB of text each. See [Rules and faults](docs/api.md#rules-and-faults).
- `[[rule]]` blocks of the `kirby-csms` configuration file take `station` (one station id) and `protocol`; the `station` key was refused before.
- `kirby-csms` stdin commands `history`, `forget`, `rules` (`list`, `set`, `clear`) and `faults` (`list`, `inject`, `remove`, `clear`), and script steps `remove_fault`, `forget`, `history`, `rules` and `faults`; `respond`, `clear_respond` and `inject_fault` steps take optional `station` and `protocol` (`inject_fault` also `capture_id`). See [Commands on stdin](docs/csms.md#commands-on-stdin).
- Action registry in `kirby-csms-core` (`kirby_csms_core::registry`): the actions of a protocol with form fields, palette texts and the station actions that rules can target, looked up by subprotocol name. The CSMS terminal UI, the API and the commands read it instead of their own lists; no behavior change for existing actions.
- TLS for the `kirby-csms` HTTP control API: `--api-tls-cert` and `--api-tls-key` (`KIRBY_CSMS_API_TLS_CERT`, `KIRBY_CSMS_API_TLS_KEY`, keys in `[api]`) serve it over HTTPS (TLS 1.2 and 1.3, HTTP/1.1), `--api-tls-client-ca` (`KIRBY_CSMS_API_TLS_CLIENT_CA`) requires a client certificate (mutual TLS). `--check-config` loads the certificate and key. Handshakes have a 5 s deadline and are limited to 64 at a time. The `api listening` line and the `api_listening` event show `https://`. See [TLS](docs/api.md#tls).
- `suite --github-annotations` prints `::error` and `::warning` workflow commands, and `--summary-md PATH` writes a Markdown summary table, appended to `$GITHUB_STEP_SUMMARY` when that variable is set. See [Run kirby-ocpp and kirby-csms in CI](docs/ci.md).
- Scenario step `reply`: answers the next CSMS CALL(s) of a command with a scripted CALLRESULT (`result`) or CALLERROR (`error` with `code`, `description`, `details`) instead of the station's handler, with `times`, `delay_ms` and `must_fire` (fail the step when the reply was not used up). The payload is checked against the response schema when the file loads; a scripted reply wins over a response rule; unused replies are removed when the scenario ends. See [reply](docs/scenarios.md#reply).
- Conformance scenario family: 15 scenarios tagged `conformance` in `conformance/scenarios` (refused tags, `Pending` and `Rejected` boot, remote start and stop, soft and hard reset, unlock, trigger messages, configuration, reservation, charging profiles, firmware and diagnostics status sequences, meter value formats, scripted replies) and the `kirby-csms` script `conformance/csms-script.toml` that drives them: `kirby-ocpp suite conformance/scenarios --tag conformance` against `kirby-csms --script conformance/csms-script.toml`. A test runs the whole family on every build. See [Conformance family](docs/scenarios.md#conformance-family) and [Run kirby-ocpp and kirby-csms in CI](docs/ci.md#conformance-job).
- Terminal UIs (emulator and `kirby-csms`): `W` lists the commands that wait for an answer with the time since they were sent, and `x` cancels the selected one (the UI stops waiting and reports `cancelled`). In the CSMS the call is abandoned, which frees the station queue after at most the call timeout; the emulator gets the new station command `cancel_call` (`Command::CancelCall`, error `CallError::Cancelled`), which forgets the oldest unanswered call of one action and ignores its late answer. `StartTransaction`, `StopTransaction` and `MeterValues` cannot be cancelled. The footer shows `W pending` and the elapsed time of the oldest command. See [Pending commands](docs/emulator.md#pending-commands).
- Monochrome terminal UIs: `--color never` or a non-empty `NO_COLOR` draws both UIs without color, using reverse video and bold plus text markers (`i`, `+`, `!`, `x` in front of log lines and notices, `◐` for a replaced station). `kirby-csms` gets `--color auto|always|never` (`KIRBY_CSMS_COLOR`); the emulator's `--color` now applies to its UI too. See [Monochrome](docs/emulator.md#monochrome).
- `--time utc|local|none` (`KIRBY_OCPP_TIME`, `KIRBY_CSMS_TIME`, `[output] time`) sets the clock in front of event and frame lines in the terminal UI logs and in human output. The default is `utc`, the same as before; `--json` timestamps and log copy and export are unchanged.
- HTTP API history cursors and filters (additive under `/v1`): every frame of `GET /v1/stations/{id}/history` has a `cursor`, the answer has `next_cursor` and `gap`, and the new parameters `since` (a cursor), `action` (a CALL and its replies), `kind` (`call`, `result`, `error`), `direction` (`in`, `out`) and `format` select frames, page forward and poll without repeats. `limit` keeps its meaning without `since`. See [History](docs/api.md#history).
- HTTP API export: `GET /v1/stations/{id}/history?format=ndjson|csv` and the new `GET /v1/transactions` (filters `station`, `state` `active|stopped`, `since`, `limit`; `format=json|csv`). CSV follows RFC 4180, prefixes cells that start with `=`, `+`, `-` or `@` with `'` and replaces control characters. See [Transactions](docs/api.md#transactions) and [CSV](docs/api.md#csv).
- HTTP API event stream resume: `GET /v1/events` honors the `Last-Event-ID` header (and `last_event_id=`), replays the events after that id from a ring of the newest 1024 events (8 MiB), then continues live; an id that is no longer held gets an `event: resync`. See [Resume](docs/api.md#resume).
- `GET /v1/metrics`: Prometheus text exposition of stations, calls received and sent by action (a fixed label set), faults, rules, dropped history frames and events, event streams, API responses by status class, uptime and build info. See [Metrics](docs/api.md#metrics).
- `kirby-csms ctl`: a command-line client of the HTTP control API for scripts and CI (`health`, `stations`, `station`, `forget`, `call`, `bulk`, `history`, `transactions`, `rules`, `faults`, `metrics`, `events`, `export`). It prints the API's JSON, takes the token from `KIRBY_CSMS_API_TOKEN` or `--token-file` (there is no token flag), speaks HTTPS with `--ca-file` and mutual TLS with `--client-cert`/`--client-key`, follows the event stream with automatic `Last-Event-ID` resume, and exits `0` on success, `1` on an API error status, `2` on a usage error, `3` when the API is unreachable or its answer is unusable and `4` when a call was delivered but a station did not answer it. See [Command-line client](docs/api.md#command-line-client).
- Session recording for the emulator: `--record FILE` writes every frame and every command of a session to a JSON-lines file (header, `frame`, `command` and `end` lines; `schema` 1, readers ignore unknown keys and line types and refuse a newer schema), in the terminal UI, headless mode, `run` and `replay`. `AuthorizationKey` values and URL passwords are redacted before they are written, also with `--show-secrets`. The file is created `0600`, an existing one is refused unless `--record-overwrite` is given, and a recording that lost data (queue full, write error, 64 MiB limit) says why and makes the command exit 1. See [Recording and replay](docs/emulator.md#recording-and-replay).
- `kirby-ocpp scenarios from-recording FILE [-o PATH]` converts a recording into a scenario file; commands without a step and dropped details are listed as warnings and comments.
- `kirby-ocpp replay FILE` sends the CALLs the emulator originated in a recording to a CSMS again, with new message ids, the recorded pauses (`--speed`, `--no-delay`) and the transaction ids of the new StartTransaction answers; it never sends a redacted credential, and reports like `run` (`--report`, `--junit`, exit codes 0, 1, 2, 130).
- Scenario steps `start_session` and `stop_session`: version-neutral aliases of `start_tx` and `stop_tx` (the old names stay).

### Changed

- Library API: `Frame`, `CsmsEvent`, `DisconnectReason`, `SubprotocolPolicy`, `ActionGroup` and `FieldType` are `#[non_exhaustive]`, so later protocol versions can add variants without a breaking release. A `match` on them from another crate needs a wildcard arm.
- `error:CODE` in response rules and faults, and `error.code` in scenario `reply` steps, also accept `FormatViolation`, `OccurrenceConstraintViolation`, `MessageTypeNotSupported` and `RpcFrameworkError`; they are sent as spelled (`ErrorCode::from_name`, `ErrorCode::ACCEPTED_EXTRA`).
- Default names use the project name: the BootNotification model is `kirby-ocpp` (was `KIRBY`), the certificate request organization without `CpoName` is `kirby-ocpp` (was `Kirby`), the `data-transfer` scenario vendor is `kirby-ocpp`, the conformance handshake vendor is `kirby-ocpp.conformance` (was `kirby.conformance`), the built-in scenario id tag is `KIRBY-OCPP-TAG` (was `KIRBY-TAG`), the Docker image user is `kirby-ocpp` and the `build.mjs` marker file is `.kirby-ocpp-npm-dist`. Scripts that match the old vendor or model need the new values.
- A non-loopback `--api-listen` now needs `--api-tls-cert` and `--api-tls-key` besides a token: a token over plain HTTP on a non-loopback address is refused at startup (exit 2). Loopback is unchanged.
- `GET /v1/rules` gains `protocol_rules` (`[{"action","protocol"}]`, `[]` when none) so global rules limited to one protocol are visible; `PUT` and `DELETE /v1/rules/{action}` accept `?protocol=` like the per-station routes. Without it both behave as before. The rule caps are documented as covering per-station rules and global rules with a protocol.
- `active_transactions[]` items of a station object carry `evse` (`null` in OCPP 1.6), and `kirby-ocpp version --json` carries `protocol_tokens` (`["ocpp1.6"]`) next to `protocols`. The API reference now states that connector keys are opaque strings and that `state` depends on the station's protocol; the configuration reference defines the top-level `protocol` of a `kirby-csms` file and limits `transaction_ids` to OCPP 1.6.
- The `id:` of the HTTP event stream is now the number of the event in one shared log (the same on every stream, one more per event) and starts from the start time of the server in microseconds instead of 1, so that an id from an earlier run is never mistaken for a current one. `state` events are sent when the view of a station changed, for all streams together.
- `kirby-ocpp state reset` refuses, and deletes nothing, when a `.bak` copy of the state or secrets file was written by a newer version, as it already did for the files themselves.
- Terminal UI key hints match what the keys do: the emulator's footer lists the log letters (`y`, `f`, `s`, `p`) only on the Logs tab and no longer hints `Enter run` on the connector list, the `kirby-csms` footer hints `/` only on the station list (the log pane lists `s`), and both help popups document `W`, the `/` rule and the markers. Tests feed every footer hint to the key handler.

### Fixed

- Secrets: recordings mask a URL password that holds `#`, `(`, `?` or other delimiters and an `AuthorizationKey` that sits in JSON text inside a string; `--print-config` and configuration files no longer let a password through in forms such as `ws:///CP:pw@host`; CALLRESULT and CALLERROR answers of `kirby-csms` calls (HTTP API, `ctl`, stdin) mask `AuthorizationKey` values unless `--show-secrets` is set. `scenarios from-recording` skips steps that hold a masked value.
- `kirby-csms` answers a WebSocket Close frame with a Close frame; `kirby-ocpp check` reports a connection ended without a close reply as a warning and bounds the close wait by `--timeout`.
- `GET /v1/stations/{id}/history` builds JSON only for the returned page; a station id over 128 bytes is a `400` on every station route; `DELETE /v1/rules/{action}?protocol=` compares and clears the rule under one lock; the OpenAPI document lists `401`, `403` and `413`.
- `kirby-csms --replay` refuses a nested transaction id in `RemoteStopTransaction` and more than 1024 recorded starts per station; a station whose CALLs are all sent may disconnect.
- An incomplete recording keeps an existing failure exit code (130, 2) instead of replacing it with 1; the emulator's `--record` refuses the configuration, TLS and report files and their aliases like `kirby-csms` does.
- `suite` row names no longer change with `--filter` or `--tag`; the Markdown summary escapes text from the CSMS; `kirby-csms --api-token-file` reads at most 64 KiB; a missing recording is reported as unreadable, not unwritable.
- Session recordings mask URL passwords in free text and long strings, preserve queued entries on overflow, and keep existing files when a replacement run fails to start. Replay handles failed StartTransaction calls, literal `${` in action names and recordings above 10000 generated steps.
- `kirby-csms ctl` bounds event-stream reconnect failures, reports reconnects without an event id, returns exit code 4 when a bulk pattern matches no station, escapes carriage returns in errors and protects export files and temporary files from unintended replacement.
- The emulator preserves a received CALLRESULT or CALLERROR when the CSMS closes the connection immediately after replying. Conformance scripts no longer need a final pause before closing.
- Recorded CALLRESULT payloads and CALLERROR details mask nested `AuthorizationKey` values, including `GetConfiguration` replies that echo the key. The wire response is unchanged, and `--show-secrets` still opts out of frame redaction.

## [0.5.0] - 2026-11-07

**The polish release: documentation as a first-class deliverable, a stronger scenario engine, a local HTTP control API for `kirby-csms`, prepared distribution channels and a long list of hardening and usability fixes.**

### 0.5.0 in numbers

| Metric | Since 0.4.0 |
| --- | --- |
| 🧾 Commits | **91** in three days (5 to 7 November 2026) |
| ✏️ Lines of code | **+21.6k / −1.6k** across 144 files; Rust grew from 70.6k to **86.9k** lines |
| 🧪 Tests | **+283** new test functions: 1,047 → **1,330** (all run in the `nextest` gate) |
| 📦 Crates | 7 → **7** (new dependencies: `axum`, `hyper`, `hyper-util`, `clap_complete`) |
| 🔎 Reviews | **11** review rounds, including product reviews that played the real binaries and both terminal UIs |
| ✨ New features | **39** |
| 🔧 Changes | **25** |
| 🐛 Fixes and hardening | **35** |

### ✨ Added

#### Scenarios and scripts

- Report vocabulary is consistent across the run, suite and fleet reports. Fleet reports and their station entries carry `scenario`, `source`, `started_at` and `duration_ms`; suite entries add `scenario` (next to `name`) and `started_at`.
- `ocpp_version` comes from each run's own report, and fleet `counts.skipped` is computed. `failed_names` and `error_names` list the two kinds of failure while `failed` keeps both.
- Emulator scenarios have variables: an optional `[vars]` table (strings, numbers, booleans) and `${name}` substitution in the string fields and JSON payloads of a step. An exact `"${name}"` keeps the JSON type, `$${` is a literal `${`, and `${station_id}` and `${index}` are built in.
- `capture = { name = "/json/pointer" }` on `send_raw`, `data_transfer` and `expect_call` steps stores a value of the answer for later steps; a missing pointer fails the step.
- `kirby-ocpp run --var NAME=VALUE` (repeatable) overrides scenario variables, also in fleet runs. A name the scenario never uses is a usage error in `run` (exit 2) and a warning in `suite`.
- Emulator scenarios have loops: `action = "repeat"` with `count` (1..=1000) and nested `[[steps.steps]]`, up to 3 levels, counters `${i}` / `${i0}` (or a custom `var`) and at most 10000 expanded steps (checked at load time). Reports and JUnit list every executed step as `name [n]` (`name [2.3]` when nested).
- Five new built-in scenarios: `heartbeat-and-status`, `metering-burst`, `data-transfer`, `security-event` and `reconnect` (see `kirby-ocpp scenarios list`).
- `kirby-ocpp suite [SOURCE...]` runs many scenarios (files, directories of `*.toml`, built-in names, `--all-builtins`) one after another, each on a fresh station. It prints a PASS/FAIL line per scenario and a summary, and supports `--report` (suite JSON), one `--junit` file with a test suite per scenario, `--fail-fast`, `--var` and a per-scenario `--timeout`.
- `kirby-csms` scripts have variables too: a top-level `[vars]` table, `${name}` substitution in `station`, `expect_status` and the JSON payloads of steps, `capture` on `call` (CALLRESULT payload) and `wait_call` (matched CALL payload), and `kirby-csms --script FILE --var NAME=VALUE`.
- `call` steps in `kirby-csms` scripts take `expect_payload = { ... }`, a JSON subset the CALLRESULT payload must contain (used with `expect_status`, not with `expect_error`).
- `kirby_ocpp_core::scenario::vars` is a public helper (`Vars`, `substitute`, `substitute_str`, `capture`) for other tools.
- Scenario and script files accept the optional top-level keys `ocpp_version` (only `"1.6"`), `schema` (only `1`) and `tags` (a list of strings, kept but not used for filtering). Other values are load errors. `evse` and `id_token` are reserved step keys.
- Every JSON report carries `schema`, `ocpp_version`, `tool_version`, `station` (not for CSMS scripts), `source`, `started_at` and a `status` string (`passed`, `failed`, `error`, `skipped`); suite and fleet reports add `counts`. Failed steps carry `action`, `params` (secrets redacted) and, for failed assertions, `expected` and `actual`. Existing fields are unchanged.
- Load errors name the file and, where the parser knows it, `path:line:column`, and suggest the closest name for an unknown action or field.

#### Control API

- `kirby-csms --api-listen ADDR` starts a local HTTP control API (JSON, plain HTTP, off by default) with `GET /v1/health`, `GET /v1/stations`, `GET /v1/stations/{id}`, `GET /v1/stations/{id}/history?limit=N`, `POST /v1/stations/{id}/calls` (same result object as the stdin commands, shared 256-command limit, 429 when full) and `DELETE /v1/stations/{id}` (forget a disconnected station).
- `GET /v1/events?station=ID` streams Server-Sent Events: one `--json` event object per message, redacted like `--json`, keep-alive every 15 s, a `note` event when the client lags, at most 16 streams (503 beyond).
- `GET /v1/rules`, `PUT`/`DELETE /v1/rules/{action}` (fixed CALLRESULT, like the `respond` script step) and `POST`/`GET`/`DELETE /v1/faults` (like `inject_fault`). Fault action names (1 to 128 bytes, no control characters), `times` and the stored-fault caps are checked identically for the API and `inject_fault` script steps.
- `--api-token` / `--api-token-file` (env `KIRBY_CSMS_API_TOKEN`, `KIRBY_CSMS_API_TOKEN_FILE`) require a bearer token. A non-loopback address needs one. Without a token, requests with an `Origin` header, `Sec-Fetch-Site: cross-site` or a non-loopback `Host` (DNS rebinding) are refused. See `docs/api.md`.
- The API caps connections (512) and the request data held by calls in flight (32 MiB). It times out slow request heads and bodies (10 s) and limits the writing of a response (60 s total, 30 s without progress; event streams end after 1 hour with a note).
- The API enables TCP keepalive, closes every connection after one response, bounds injected faults (1000 faults, `times` up to 1000000, 8 MiB) and ends event streams after the shutdown events.
- New direct dependencies: `axum` (HTTP/1 only), `hyper` and `hyper-util`.
- The API shapes are a versioned contract (`api_version` 1, in `GET /v1` and `GET /v1/health`; breaking changes will ship as `/v2`). `GET /v1` lists every route, `OPTIONS` on a known path answers `204` with `Allow` (no CORS headers by design), and every SSE message has an `id:` line (`Last-Event-ID` is ignored). Station objects and `connected` events carry `protocol` (the negotiated subprotocol, `null` if none) and `connected` events carry `remote_addr`; the same fields appear in `kirby-csms --json` and `{"cmd":"stations"}` output, and the text line reads `CP-1 connected from 10.0.0.5:4242 (ocpp1.6, profile 2)`.
- API errors have the body `{"error":{"code":"...","message":"..."}}` with stable snake_case codes (table in `docs/api.md`). A call to a station that was never seen is `404` `unknown_station`; to a known one that is disconnected, `409` `station_not_connected`.
- `POST /v1/stations/{id}/calls` answers `{"outcome":"result"|"call_error"|"timeout","ok":...,...}` without `type`, `station` and `action`; CALLERROR and timeout stay HTTP `200`. The stdin answers are unchanged.
- Transaction ids (`active_transactions[].id`, `state.transactions[].id`) are strings in the API (OCPP 2.x ids are strings); the stdin answers and `--json` lines keep numbers.

#### Whitepaper and offline behavior

- Transaction-related `MeterValues` samples taken while the station is offline (or while the transaction still has a local id) are queued instead of dropped: bounded (1000, oldest first), persisted, and sent with the real `transactionId` after the start was synchronized and before the stop. State file format 8 (`offline_samples`; older files load).
- Security profile fallback (`A05.FR.08`): after a change of `SecurityProfile` the station goes back to the previous profile (and `AuthorizationKey`) after 3 connection failures the profile can explain, queues a `ReconfigurationOfSecurityParameters` event and reconnects. Only HTTP 401/403 and client-certificate TLS alerts count, never server-certificate errors, closes, timeouts, refused TCP or DNS, so a man in the middle cannot force a downgrade. New flag `--security-profile-fallback <BOOL>` (`OCPP_SECURITY_PROFILE_FALLBACK`, default `true`). State file format 9 (`pending_profile_switch`, previous key in the secrets file; older files load).
- Client certificate alternation (`A02.FR.28`) no longer depends on a TLS alert: an HTTP 401/403 upgrade answer or a close/reset right after the TLS handshake switches between the new and the previous certificate at once, and any other failure does so after 3 in a row. A bad renewed certificate cannot strand the station.

#### Distribution

- New release target `aarch64-unknown-linux-musl` (static, built on `ubuntu-24.04-arm`) and the npm platform package `@archhaze24/kirby-ocpp-linux-arm64-musl`. The npm launcher prefers it on musl arm64 systems and falls back to it when the glibc package is missing. A release has 14 files (7 archives + 7 `.sha256`).
- Multi-arch container image `ghcr.io/archhaze24/kirby-ocpp` (`linux/amd64`, `linux/arm64`) built by the release workflow from the static musl binaries: non-root user, state volume `/data`, port 9000, both binaries (`kirby-csms` as the first argument starts the CSMS). `Dockerfile.dev` builds it from a source checkout.
- Homebrew formula and Scoop manifest templates under `packaging/` plus `.github/scripts/render-packaging.sh`; the release workflow renders them with the real checksums into the workflow artifact `packaging`.

#### Emulator CLI

- `kirby-ocpp version` prints the tool version, git commit, build date, target triple and supported protocols (`--json` gives one JSON object); `--version` stays one line. A `build.rs` embeds the commit and the date (`SOURCE_DATE_EPOCH` is honored).
- `kirby-ocpp completions <bash|zsh|fish|powershell|elvish>` prints a shell completion script (new dependency `clap_complete`).
- `version --help`, `completions --help` and `scenarios --help` no longer repeat the station flags (they still parse there); `run --help` and `suite --help` point to `kirby-ocpp --help` for them.
- `kirby-ocpp scenarios validate FILE|DIR|NAME...` parses scenarios without connecting: `ok PATH (N steps)` or the load error with the path, exit 0 or 2.
- `-v/--verbose`, `-q/--quiet` and `--color auto|always|never` (`NO_COLOR` is honored) for `run` and `suite` output, and `run --persist`.
- Connection errors carry a one-line hint: connection refused, TLS against a plain server, unknown CA, HTTP 400/401/403/404 and subprotocol refusals (with the offered subprotocol).
- `--help` groups the flags under Connection, Identity, Station, TLS, Reconnect, Fleet, Faults, Output and Scenarios.

#### Documentation

- `README.md` is a landing page; detailed pages live under `docs/` (index in `docs/README.md`): architecture, emulator, CSMS, scenarios and scripts, security, configuration reference, testing, development, distribution and the HTTP control API (`docs/api.md`).

### 🔧 Changed

- Station objects of the control API (and of `kirby-csms --json` and `{"cmd":"stations"}`) always carry every field: `remote_addr`, `vendor` and `model` are `null` while unknown, `connectors` is `{}`, `active_transactions` is `[]` and `security_events` is `0` before the engine has state; `GET /v1/stations/{id}` always has `state` (`null` without state). A key in a shape is always present; a key that is missing is unknown to this version (`docs/api.md`).
- `kirby-csms` scripts reject unknown top-level keys (they were ignored before), like emulator scenarios. The allowed keys are `name`, `description`, `vars`, `steps`, `ocpp_version`, `schema` and `tags`.
- JUnit test case names are the step's stable label (`expect_call Heartbeat out`, `start_tx c1 TAG`) instead of `<number>. <name>`, `classname` is `ocpp1.6.<scenario>`, the position moves to a test case property and each suite has properties (`ocpp_version`, `station`, `tool_version`, `source`). Duplicate labels in a suite get ` #2`.
- `StepReport` and `ScenarioReport` in `kirby-ocpp-core` have new fields; build them with `StepReport::new` and `ScenarioReport::new`.
- Headless mode, `run` and `suite` exit with code 1 when the station gives up reconnecting (`--no-reconnect`, or `--reconnect-max-attempts` used up), printing `reconnect stopped (...)` with the last connect error; before, the process idled until interrupted or timed out. A fleet exits 1 when every station gave up. The station snapshot has a new field `reconnect_stopped` (reason text or `null`) that records the give-up explicitly.
- `run` and `suite` print only the report by default; the frame and log lines need `-v` (`--json` still prints events). `-q` prints only errors and the verdict line.
- `run` is ephemeral like `suite`: it neither reads nor writes the state file unless `--persist` is given.
- A missing `--url` shows an example and names `kirby-csms --listen 127.0.0.1:9000`; an unknown `--scenario` name says so and suggests the closest built-in.
- Emulator TUI: `q` and `Ctrl-C` ask for confirmation (default No) while a scenario is running, a connector has an active transaction or the fleet has more than one station; a second `Ctrl-C` quits at once. Otherwise they quit immediately.
- Both TUIs: the footer always keeps the `? help` and `: palette` (`: actions` in the CSMS) hints, and the help popups start with a "First steps" paragraph.
- `kirby-csms` TUI: the empty state shows the listen address, a station URL to copy and, at security profile 0, a note that any peer can connect.
- `kirby-csms` prints `stations connect to ws://<host>:<port>/<prefix>/<station id>` and, for security profile 0 on a non-loopback address, a warning, after the `listening on ...` line (the hint is omitted with `--json`). Headless runs (`--no-tui`, `--script`, a pipe) print these startup lines on stdout through the ordered event queue, so `kirby-csms ... > csms.log 2>&1` always holds whole lines with `listening on ...` first; with `--json` the warning is `{"type":"warning","text":...}`. Under the TUI they stay on stderr.
- The emulator fails at startup with exit 2 when `--tls-ca`, `--tls-cert` or `--tls-key` cannot be read, when `--vendor`, `--model`, `--serial` or `--firmware-version` is longer than OCPP 1.6 allows, or when `--id` contains control or invisible characters. The message names the flag. Before, such a station retried or reconnected forever.
- A TLS trust failure reads `TLS error: ...` instead of "TLS configuration error", and a `ws://` URL against a TLS server gets the hint to try `wss://`.
- `kirby-csms` names the address when it cannot bind the listener. `CsmsEvent::Frame` copies above 64 KiB carry `{"truncated":true,"originalBytes":N}` (the history already did), which bounds the memory a stalled event consumer can pin. `malformed` events that mention `AuthorizationKey` are redacted.
- `run` and `suite` PASS lines show milliseconds below one second. `version --help` has its own `--json` text, and `--licenses` and the help use US spelling.
- Emulator TUI: the footer shows `waiting... <action>` while a command is waiting for the CSMS.
- `kirby-csms-core`: the per-station call queue limit (1024) is reserved when `call()` is made, so `QueueFull` is returned at once even when the session is blocked writing to a station that does not read. `try_inject` keeps a running byte total instead of re-serializing all faults. New `forget_if_disconnected` and `history_tail`.
- Control API connections are served by hyper itself. The request-head timeout is hyper's `header_read_timeout`, so blank lines or odd line endings no longer confuse it. `Host` also accepts `localhost.`, `[0:0:0:0:0:0:0:1]` and the bound address, and `limit=+5` is `400`.
- `kirby-csms-core`: the timeout of `CsmsHandle::call` counts from the call and so covers the time spent queued. A call that waited longer fails with `Timeout` without being written.
- `kirby-csms-core`: a call whose caller is gone (its future was dropped) no longer holds the station's queue until its own timeout. The session waits at most the default call timeout for the abandoned CALL, then writes the next one; a late answer to the abandoned one is ignored. `CsmsHandle::clear_responder` returns whether a responder was removed.
- Scenario files must be regular files of at most 1 MiB, and an existing path beats a built-in of the same name. A substitution is capped at about 1 MiB and CSMS values in failure reasons are cut to about 200 characters.
- `suite` runs a file named twice once, writes its reports on SIGINT and SIGTERM too, and keeps a signal that arrives between scenarios.
- Back-to-back CALLs of the station (queued offline samples, then their `StopTransaction`) are queued on the connection in call order; before, the order depended on the scheduler of a multi-thread runtime.
- Faster builds and tests: line-table debuginfo in the dev profile and one integration-test binary per crate (cold test build 3:07 to 1:23, warm test run 64 s to 33 s; `cargo nextest` runs the suite in about 15 s).

### 🐛 Fixed

- The control API sets `Allow` in one place, derived from the route table: only on a `405` and on the `204` answer to `OPTIONS`, as `GET, HEAD, OPTIONS` (sorted, comma and space). `401`, `403` and other answers no longer carry axum's own `Allow`.
- A repeated query parameter of the control API (`limit=1&limit=2`, `station=a&station=b`) is `400` `bad_request` instead of silently taking the last value.
- `--id .` and `--id ..` are refused (exit 2): the URL layer drops such path segments, so the station connected to the bare base path under the wrong id. `--ws-subprotocol` must be an RFC 6455 token. Non-UTF-8 percent-encoded bytes in URL credentials reach the Basic `Authorization` header unchanged, an unparsable URL in an error text loses everything before its last `@`, and the status line of a refused upgrade is sanitized and cut to 200 characters in the error.
- A secrets file that is unreadable or written by a newer build is never overwritten; the station keeps running without persisting secrets.
- The emulator keeps the queue entry of a still running offline transaction when the queue overflows, closes an unkept start with a matching stop reason, and ends delayed-reply tasks with the connection.
- A restored sample queue drops samples of a transaction that no longer exists.
- A retried `MeterValues` sample no longer arrives after the `StopTransaction` of its transaction, and a request that fails schema validation or serialization is not retried. The connection task prunes abandoned queued CALLs in amortized constant time.
- Security events stay in the persisted queue until the CSMS answered, so a restart with events in flight no longer loses them.
- A scenario is reported as `cancelled` only when a step was cut short. Scenario files load off the UI loop, and footer notices are sanitized in both TUIs.
- In `kirby-csms` scripts, `wait_call` fails at once after lost events only for waits that started before the loss.
- Stdin commands and the control API share one command executor in `kirby-csms`.
- Release tooling: the npm builder verifies archives against their `.sha256` assets, the launcher does not forward SIGINT twice on a terminal, the container image is pushed only after the GitHub release succeeded, and `latest` only moves for the highest final version.
- Windows CI test made independent of the OS send buffer size.
- Connection error text from the central system is sanitized in every stderr line and in the fleet report. `run` and `suite` end with `connection lost: ...` (exit 1) when the link is gone for good while a scenario runs, and the headless give-up check no longer mistakes a short deliberate disconnect for it.
- Ctrl-C inside a form or browser asks before quitting when work would be lost, and declining brings the view back; it no longer replaces another open question. `q` also confirms the quit question, a notice takes the footer line before the `waiting...` marker, and the marker clears when the connection drops.
- `kirby-ocpp version` reports a commit only for a checkout of this workspace (not an enclosing repository), and rebuilds on worktrees and packed refs.
- Connection hints: 401 and 403 point to the credentials and the security profile, a missing client certificate to `--tls-cert`/`--tls-key`, a name mismatch to `--tls-server-name`.
- `run` notes an ignored `--state-dir`, and `scenarios validate` checks a repeated file once, warns about a scenario name used twice and notes skipped non-regular `*.toml` entries. The `kirby-csms` TUI builds the example station URL correctly for addresses without a port and for IPv6.
- Scenario and script reports and JUnit output redact an `AuthorizationKey` value in the expected and actual payloads and in the failure reason, not only in `params`. Script reports show resolved `params`.
- A scenario or script written for a later `schema` or another `ocpp_version` fails with the version message even when it uses newer keys; `schema`, `ocpp_version` and `tags` of the wrong type and the reserved `evse` and `id_token` step keys get dedicated messages. `kirby-csms --script` reads files with the same size limit and regular-file check as scenarios.
- Stderr and stdout lines of the emulator are no longer interleaved mid-line when both go to one pipe, the last connection error survives a lagging event stream when the emulator gives up, and the generic `BootNotification accepted` line is gone (the boot handler's own line stays). The invisible and bidirectional marks U+200B to U+200F, U+061C and U+FEFF count as unsafe in station ids, fault actions and printed text.
- `expect_call` with `direction = "in"` sees CALLs that arrive between the connection and the start of the scenario (`run`, `suite` and fleet runs subscribe before they connect), and a raw `BootNotification` logs its result again.
- Shell completion matches `--help` (no station flags under `version`, `completions` or `scenarios`), `--print-config` works with unreadable TLS files, and the final `error:` line no longer overtakes queued output.
- `--var` numbers keep their written form inside text (`1.50` stays `1.50`); an exact `"${n}"` stays typed. An empty step `name` uses the derived label, and `expect = {error = true, ...}` combined with `status` or `payload` is a load error.
- `--report` and `--junit` paths are checked before the run (exit 2 for identical paths, a scenario file, a missing or unwritable directory) and written atomically. JUnit suites of setup errors carry `source` and `time`, and invisible characters are removed from JUnit names.
- `kirby-csms` scripts match and capture the payload of station CALLs above 64 KiB (read from the frame history; the step fails with `payload truncated (N bytes)` if the history no longer holds the frame). A `respond` step needs a table payload (checked at load). A connection refused for an invalid station id (over 128 bytes, control characters, empty) is reported as `auth_failed` with `station: null`; `kirby-csms-core` exports `event_frame`. `note:` prefixes the `stations connect to ...` line.
- Both TUIs: help popups wrap to the terminal width and the first-steps text says the station connects at startup. `AuthorizationKey` fields are masked while typing, long form errors wrap and the input-cap notice is no longer hidden by another error. Long station ids are shortened in the middle, the too-small notice reads `need WxH, have WxH`, the `errors` log filter no longer includes warnings, and a disconnected CSMS station's transaction counter is dimmed.
- JUnit test case names stay unique when an explicit label looks like a generated ` #N` one, and the `classname` has no control characters. Did-you-mean suggestions are bounded in time and length, and error positions inside a multi-byte character no longer drop the line and column.

### 🔒 Security

- TUI forms (both programs): DataTransfer `vendorId`, `messageId` and `data` are length-checked inline (255 characters, 50 characters, 64 KiB). Every field takes at most 256 KiB, with a visible notice when input was dropped.
- ChangeConfiguration values and DataTransfer `data` keep leading and trailing spaces. `kirby-csms` asks before a ClearChargingProfile with no filter, which clears every profile.
- The emulator's log export tightens an existing file to mode 0600 before writing and refuses a symlink.
- `kirby-csms --auth-keys-file` warns when the file is shared, ignores a UTF-8 BOM, keeps trailing spaces of a key and refuses a file over 1 MiB.
- `--log-file` is created with mode 0600 on unix.
- `kirby-csms-core` caps sockets in the TLS/WebSocket handshake (`max_pending_handshakes`, 1024) and bounds the per-station CALL queue (`CsmsError::QueueFull`).
- Station-supplied text in the CSMS state is cut to 255 characters, and station ids with bidi or line-separator characters are refused with HTTP 400.

## [0.4.0] - 2026-11-05

**kirby-csms joins kirby-ocpp: a full central system for your terminal, the OCPP 1.6 Security
Whitepaper on both sides, and a much harder, faster, better tested code base.**

### 0.4.0 in numbers

| Metric | Since 0.3.0 |
| --- | --- |
| 🧾 Commits | **39** in four days (2 to 5 November 2026) |
| ✏️ Lines of code | **+29,821 / −851** across 197 files; Rust grew from 43.0k to **70.6k** lines |
| 🧪 Tests | **+324** new test functions: 723 → **1,047** (1,049 tests in the full gate) |
| 📦 Crates | 4 → **7** (`kirby-csms`, `kirby-csms-core`, `kirby-csms-tui` are new) |
| 🔎 Reviews | 2 deep adversarial reviews and 4 focused ones: **over 60 verified findings fixed** |
| ✨ New features | **24** |
| 🔧 Changes | **9** |
| 🐛 Fixes and hardening | **30** |

### ✨ Added

#### kirby-csms: a standalone OCPP 1.6J central system

New binary `kirby-csms` (default listen `0.0.0.0:9000`) and the new crates `kirby-csms-core`
(the engine, usable as a library) and `kirby-csms-tui`.

- Multi-station engine: stations connect to `ws(s)://host:port/<any prefix>/<station id>`, with a
  bounded per-station frame history (entry and byte caps) and an event stream.
- Per-station state read model: boot information, connectors, transactions with meter readings,
  security events, log and firmware status, installed certificates.
- Typed, schema-checked commands for every central-system action, the Security Whitepaper ones
  included, plus a raw `call` for anything else.
- Scripted replies: response rules, fault injection and custom responders, to test how firmware
  copes with a misbehaving central system.
- Validation of inbound CALLs against the bundled OCPP schemas, answered with the right
  CALLERROR code (`--validate-inbound`, on by default).
- Server-side WebSocket ping/pong watchdog (`--ping-interval`, `--pong-timeout`).
- Connection security profiles 1 to 3: Basic Auth (`--auth-key`, `--auth-keys-file`), TLS and
  mutual TLS with the certificate CN equal to the station id.
- Headless mode: `--json`, `--log-file`, JSON-lines commands on stdin, TOML `--script` with
  `--report` and `--junit`, documented exit codes.
- Terminal UI: station list and details, merged or per-station log, command forms for every
  action, response rules and security forms.
- Shipped next to `kirby-ocpp` in every release archive and as the npm package
  `kirby-csms`.

#### OCPP 1.6 Security Whitepaper (edition 4) on the emulator

- 11 new messages with the 22 official schemas: `CertificateSigned`, `DeleteCertificate`,
  `ExtendedTriggerMessage`, `GetInstalledCertificateIds`, `GetLog`, `InstallCertificate`,
  `LogStatusNotification`, `SecurityEventNotification`, `SignCertificate`,
  `SignedFirmwareStatusNotification`, `SignedUpdateFirmware`.
- Certificate store with OCSP-style certificate hashes and `CertificateStoreMaxLength`.
- Security profiles 0 to 3 on the station: Basic Auth with `AuthorizationKey`, TLS, mTLS, upgrade
  rules, optional downgrade (`AllowSecurityProfileDowngrade`) and a reconnect after every change
  of security parameters.
- Client certificates: `SignCertificate` / `CertificateSigned` with chain checks, repeat with
  back-off (`CertSigningWaitMinimum`, `CertSigningRepeatTimes`) and alternation between the new
  and the previous certificate.
- Security events: queued, persisted and reported, including TLS version, cipher suite and
  certificate failures, reported once per series of failed attempts.
- `AdditionalRootCertificateCheck`: central system root rotation signed by the old root, with a
  fallback until the first successful connection (`--additional-root-certificate-check`).
- `AllowCentralSystemTLSWildcards`: wildcard server certificates are rejected unless allowed.
- `GetLog` and `SignedUpdateFirmware` simulated with their status notifications.
- Emulator TUI actions and the scenario steps `request_certificate` and `security_event`.

#### Reports and secrets

- JUnit XML for scenario runs: `kirby-ocpp run --junit`, single station and fleet.
- The `AuthorizationKey` is redacted in recorded frames (logs, TUI, `--json`, history);
  `--show-secrets` turns it off.

#### Release engineering

- Release notes are taken from this file by the release workflow; a release without notes fails
  early, also in dry runs.
- Tag and Cargo version must match; prerelease tags (`v1.2.3-rc.1`) publish as GitHub prereleases.
- Every crate ships a `LICENSE`; the library crates have their own README and crates.io metadata.

### 🔧 Changed

- State file format 3 → 7 (older files still load).
- `StopTransactionOnInvalidId` and `MaxEnergyOnInvalidId` now follow the spec: they apply to the
  `StartTransaction` answer, not to `Authorize`. `StopTransactionOnInvalidId` defaults to `true`.
- The central system answers an unknown action with `NotImplemented` instead of `NotSupported`.
- `kirby-ocpp --json` implies headless mode, and `--log-file` also works with the TUI.
- Bounded memory everywhere it was unbounded: TUI logs (byte budget, large frames truncated),
  CSMS history, script backlog, scenario frames, stdout and log-file queues.
- `GetCompositeSchedule` is bounded to 7 days and 1,000 periods.
- OCPP schema attribution now points to the official Open Charge Alliance downloads
  (CC BY-ND 4.0); the bundled files are byte-identical to them.
- Faster builds and tests: line-table debuginfo in the dev profile and one integration-test
  binary per crate (cold test build 3:07 → 1:23, warm test run 64 s → 33 s on a
  development machine; `cargo nextest` runs the suite in about 15 s).
- Windows CI test made independent of the OS send buffer size.

### 🐛 Fixed

#### Station

- Energy was credited with a stale "charging" flag after status changes.
- A `Stopping` transaction restored from disk was reported with the date 1970 or lost when two
  connectors shared one transaction id.
- Failed queued `StartTransaction` / `StopTransaction` messages were never retried; the offline
  queue is now retried and capped at 1,000 entries.
- `GetDiagnostics` and `UpdateFirmware` shared one status queue, so firmware updates never
  reported `Installed`.
- Clock-aligned `MeterValues` starved connectors on stations with more than 16 connectors.
- A pending `ChangeAvailability` could stay stuck after a start that never happened.
- `RemoteStartTransaction` ignored a parent id tag that only the central system's `Authorize`
  answer carried.
- Periodic samples stopped while a transaction was suspended; intervals above 24 h sampled every
  24 h; an online sync forced a resend of all status notifications.
- Stale firmware and diagnostics status after a disconnect.
- The `WebSocketPingInterval` key was stored but unused.
- A write that never completed could hang the connection: writes now time out.
- Messages from the central system are limited to 4 MiB; a huge `--call-timeout-ms` no longer
  panics.
- A05.FR.01: setting the current security profile again is rejected as the whitepaper demands.
- Overlapping `SignCertificate` requests could restore a stale private key.

#### Central system

- A dropped `call` future freed the outstanding-CALL slot early.
- `max_stations` could be exceeded by parallel handshakes.
- An empty configured key accepted an empty password.
- The `--timeout` of `--script` could overflow and panic.

### 🔒 Security hardening

- Unbounded memory growth from unauthenticated stations closed (byte caps, action key length,
  station id limits).
- Text sent by the other side (station ids, payloads, reasons, certificate subjects) is
  sanitized before it reaches terminals, logs and reports; C1 controls and bidi controls
  included. A station can no longer forge lines in a script report.
- Secrets stay out of output: URL passwords (`--url`, `--help`, environment echo), `--auth-key`
  in clap errors, `AuthorizationKey` in schema error messages and `Debug` output.
- Certificate checks: a self-signed root needs a valid signature, a `CertificateSigned` chain
  must be signed link by link, wildcard server certificates are rejected by default.
- The secrets file is written before the state file and a corrupt one is moved aside to `.bak`
  instead of being overwritten.
- Release workflow: least-privilege token permissions, third-party actions pinned to commit
  SHAs, no expression injection into shell steps, tag/version check.
- The npm launcher forwards signals to the native binary, and `npm/build.mjs` refuses to wipe
  directories that are not its own output.

### 🛟 Reliability

- A closed stdout (`| head`) no longer kills a run before its JUnit report is written.
- An unread stdout or a stalled log file no longer blocks the process or the log file.
- A clipboard helper that never exits no longer blocks the exit of the TUI.
- SIGTERM during `kirby-csms --script` still writes the JUnit report.
- A scenario `lagged` failure no longer sticks for the rest of the run.

[Unreleased]: https://github.com/archhaze24/kirby-ocpp/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/archhaze24/kirby-ocpp/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/archhaze24/kirby-ocpp/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/archhaze24/kirby-ocpp/compare/v0.3.0...v0.4.0
