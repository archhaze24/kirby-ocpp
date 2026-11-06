# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). The release workflow publishes the section of the
tag as the GitHub release notes.

## [Unreleased]

### ✨ Added

#### Scenarios and scripts

- Emulator scenarios have variables: an optional `[vars]` table (strings, numbers, booleans) and `${name}` substitution in the string fields and JSON payloads of a step. An exact `"${name}"` keeps the JSON type, `$${` is a literal `${`, and `${station_id}` and `${index}` are built in.
- `capture = { name = "/json/pointer" }` on `send_raw`, `data_transfer` and `expect_call` steps stores a value of the answer for later steps; a missing pointer fails the step.
- `kirby-ocpp run --var NAME=VALUE` (repeatable) overrides scenario variables, also in fleet runs. A name the scenario never uses is a usage error in `run` (exit 2) and a warning in `suite`.
- Emulator scenarios have loops: `action = "repeat"` with `count` (1..=1000) and nested `[[steps.steps]]`, up to 3 levels, counters `${i}` / `${i0}` (or a custom `var`) and at most 10000 expanded steps (checked at load time). Reports and JUnit list every executed step as `name [n]` (`name [2.3]` when nested).
- Five new built-in scenarios: `heartbeat-and-status`, `metering-burst`, `data-transfer`, `security-event` and `reconnect` (see `kirby-ocpp scenarios list`).
- `kirby-ocpp suite [SOURCE...]` runs many scenarios (files, directories of `*.toml`, built-in names, `--all-builtins`) one after another, each on a fresh station. It prints a PASS/FAIL line per scenario and a summary, and supports `--report` (suite JSON), one `--junit` file with a test suite per scenario, `--fail-fast`, `--var` and a per-scenario `--timeout`.
- `kirby-csms` scripts have variables too: a top-level `[vars]` table, `${name}` substitution in `station`, `expect_status` and the JSON payloads of steps, `capture` on `call` (CALLRESULT payload) and `wait_call` (matched CALL payload), and `kirby-csms --script FILE --var NAME=VALUE`.
- `call` steps in `kirby-csms` scripts take `expect_payload = { ... }`, a JSON subset the CALLRESULT payload must contain (used with `expect_status`, not with `expect_error`).
- `kirby_ocpp_core::scenario::vars` is a public helper (`Vars`, `substitute`, `substitute_str`, `capture`) for other tools.

#### Control API

- `kirby-csms --api-listen ADDR` starts a local HTTP control API (JSON, plain HTTP, off by default) with `GET /v1/health`, `GET /v1/stations`, `GET /v1/stations/{id}`, `GET /v1/stations/{id}/history?limit=N`, `POST /v1/stations/{id}/calls` (same result object as the stdin commands, shared 256-command limit, 429 when full) and `DELETE /v1/stations/{id}` (forget a disconnected station).
- `GET /v1/events?station=ID` streams Server-Sent Events: one `--json` event object per message, redacted like `--json`, keep-alive every 15 s, a `note` event when the client lags, at most 16 streams (503 beyond).
- `GET /v1/rules`, `PUT`/`DELETE /v1/rules/{action}` (fixed CALLRESULT, like the `respond` script step) and `POST`/`GET`/`DELETE /v1/faults` (like `inject_fault`).
- `--api-token` / `--api-token-file` (env `KIRBY_CSMS_API_TOKEN`, `KIRBY_CSMS_API_TOKEN_FILE`) require a bearer token. A non-loopback address needs one. Without a token, requests with an `Origin` header, `Sec-Fetch-Site: cross-site` or a non-loopback `Host` (DNS rebinding) are refused. See `docs/api.md`.
- The API caps connections (512) and the request data held by calls in flight (32 MiB), times out slow request heads and bodies (10 s), limits the writing of a response (60 s total, 30 s without progress; event streams end after 1 hour with a note), enables TCP keepalive, closes every connection after one response, bounds injected faults (1000 faults, `times` up to 1000000, 8 MiB) and ends event streams after the shutdown events.
- New dependency: `axum` (HTTP/1 only) with `hyper` and `tower` crates.

#### Whitepaper and offline behavior

- Transaction-related `MeterValues` samples taken while the station is offline (or while the transaction still has a local id) are queued instead of dropped: bounded (1000, oldest first), persisted, and sent with the real `transactionId` after the start was synchronized and before the stop. State file format 8 (`offline_samples`; older files load).
- Security profile fallback (`A05.FR.08`): after a change of `SecurityProfile` the station goes back to the previous profile (and `AuthorizationKey`) after 3 connection failures the profile can explain, queues a `ReconfigurationOfSecurityParameters` event and reconnects. Only HTTP 401/403 and client-certificate TLS alerts count, never server-certificate errors, closes, timeouts, refused TCP or DNS, so a man in the middle cannot force a downgrade. New flag `--security-profile-fallback <BOOL>` (`OCPP_SECURITY_PROFILE_FALLBACK`, default `true`). State file format 9 (`pending_profile_switch`, previous key in the secrets file; older files load).
- Client certificate alternation (`A02.FR.28`) no longer depends on a TLS alert: an HTTP 401/403 upgrade answer or a close/reset right after the TLS handshake switches between the new and the previous certificate at once, and any other failure does so after 3 in a row. A bad renewed certificate cannot strand the station.

#### Distribution

- New release target `aarch64-unknown-linux-musl` (static, built on `ubuntu-24.04-arm`) and the npm platform package `@archhaze24/kirby-ocpp-linux-arm64-musl`. The npm launcher prefers it on musl arm64 systems and falls back to it when the glibc package is missing. A release has 14 files (7 archives + 7 `.sha256`).
- Multi-arch container image `ghcr.io/archhaze24/kirby-ocpp` (`linux/amd64`, `linux/arm64`) built by the release workflow from the static musl binaries: non-root user, state volume `/data`, port 9000, both binaries (`kirby-csms` as the first argument starts the CSMS). `Dockerfile.dev` builds it from a source checkout.
- Homebrew formula and Scoop manifest templates under `packaging/` plus `.github/scripts/render-packaging.sh`; the release workflow renders them with the real checksums into the workflow artifact `packaging`.

#### Documentation

- `README.md` is a landing page; detailed pages live under `docs/` (index in `docs/README.md`): architecture, emulator, CSMS, scenarios and scripts, security, configuration reference, testing, development, distribution and the HTTP control API (`docs/api.md`).

### 🔧 Changed

- `kirby-csms-core`: the per-station call queue limit (1024) is reserved when `call()` is made, so `QueueFull` is returned at once even when the session is blocked writing to a station that does not read. `try_inject` keeps a running byte total instead of re-serializing all faults. New `forget_if_disconnected` and `history_tail`.
- Control API connections are served by hyper itself (`hyper` and `hyper-util` are now direct dependencies, already part of the axum tree). The request-head timeout is hyper's `header_read_timeout`, so blank lines or odd line endings no longer confuse it. `Host` also accepts `localhost.`, `[0:0:0:0:0:0:0:1]` and the bound address, and `limit=+5` is `400`.
- `kirby-csms-core`: the timeout of `CsmsHandle::call` counts from the call and so covers the time spent queued (a call that waited longer fails with `Timeout` without being written), and a call whose caller is gone (its future was dropped) no longer holds the station's queue until its own timeout: the session waits at most the default call timeout for the abandoned CALL, then writes the next one; a late answer to the abandoned one is ignored. `CsmsHandle::clear_responder` returns whether a responder was removed.
- Scenario files must be regular files of at most 1 MiB, and an existing path beats a built-in of the same name. A substitution is capped at about 1 MiB and CSMS values in failure reasons are cut to about 200 characters.
- `suite` runs a file named twice once, writes its reports on SIGINT and SIGTERM too, and keeps a signal that arrives between scenarios.
- Back-to-back CALLs of the station (queued offline samples, then their `StopTransaction`) are queued on the connection in call order; before, the order depended on the scheduler of a multi-thread runtime.
- Faster builds and tests: line-table debuginfo in the dev profile and one integration-test binary per crate (cold test build 3:07 to 1:23, warm test run 64 s to 33 s; `cargo nextest` runs the suite in about 15 s).

### 🐛 Fixed

- `--id .` and `--id ..` are refused (exit 2): the URL layer drops such path segments, so the station connected to the bare base path under the wrong id. `--ws-subprotocol` must be an RFC 6455 token. Non-UTF-8 percent-encoded bytes in URL credentials reach the Basic `Authorization` header unchanged, an unparsable URL in an error text loses everything before its last `@`, and the status line of a refused upgrade is sanitised and cut to 200 characters in the error.
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

### 🔒 Security

- TUI forms (both programs): DataTransfer `vendorId`, `messageId` and `data` are length-checked inline (255 characters, 50 characters, 64 KiB); every field takes at most 256 KiB, with a visible notice when input was dropped; ChangeConfiguration values and DataTransfer `data` keep leading and trailing spaces; `kirby-csms` asks before a ClearChargingProfile with no filter, which clears every profile.
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

[Unreleased]: https://github.com/archhaze24/kirby-ocpp/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/archhaze24/kirby-ocpp/compare/v0.3.0...v0.4.0
