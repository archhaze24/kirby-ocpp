# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). The release workflow publishes the section of the
tag as the GitHub release notes.

## [Unreleased]

Work in progress after 0.4.0.

### Scenarios

- Emulator scenarios have variables: an optional `[vars]` table (strings, numbers, booleans), `${name}` substitution in the string fields and JSON payloads of a step (an exact `"${name}"` keeps the JSON type, `$${` is a literal `${`), and the built-ins `${station_id}` and `${index}`.
- `capture = { name = "/json/pointer" }` on `send_raw`, `data_transfer` and `expect_call` steps stores a value of the answer for later steps; a missing pointer fails the step.
- `kirby-ocpp run --var NAME=VALUE` (repeatable) overrides scenario variables, also in fleet runs.
- `kirby_ocpp_core::scenario::vars` is a public helper (`Vars`, `substitute`, `substitute_str`, `capture`) for other tools.
- `kirby-csms` scripts have variables too: a top-level `[vars]` table, `${name}` substitution in `station`, `expect_status` and the JSON payloads of steps, `capture = { name = "/json/pointer" }` on `call` (CALLRESULT payload) and `wait_call` (matched CALL payload), and `kirby-csms --script FILE --var NAME=VALUE`.
- `call` steps in `kirby-csms` scripts take `expect_payload = { ... }`, a JSON subset the CALLRESULT payload must contain (with `expect_status`; not with `expect_error`).
- Emulator scenarios have loops: `action = "repeat"` with `count` (1..=1000) and nested `[[steps.steps]]`, up to 3 levels, counters `${i}` / `${i0}` (or a custom `var`), at most 10000 expanded steps (checked at load time). Reports and JUnit list every executed step as `name [n]` (`name [2.3]` when nested).
- Five new built-in scenarios: `heartbeat-and-status`, `metering-burst`, `data-transfer`, `security-event` and `reconnect` (see `kirby-ocpp scenarios list`).
- `kirby-ocpp suite [SOURCE...]` runs many scenarios (files, directories of `*.toml`, built-in names, `--all-builtins`) one after another, each on a fresh station, with a per-scenario PASS/FAIL line, a summary, `--report` (suite JSON), one `--junit` file with a test suite per scenario, `--fail-fast`, `--var` and a per-scenario `--timeout`.

### Control API

- `kirby-csms --api-listen ADDR` starts a local HTTP control API (JSON, plain HTTP, off by default) with `GET /v1/health`, `GET /v1/stations`, `GET /v1/stations/{id}`, `GET /v1/stations/{id}/history?limit=N`, `POST /v1/stations/{id}/calls` (same result object as the stdin commands, shared 256-command limit, 429 when full) and `DELETE /v1/stations/{id}` (forget a disconnected station). `--api-token` / `--api-token-file` (env `KIRBY_CSMS_API_TOKEN`, `KIRBY_CSMS_API_TOKEN_FILE`) require a bearer token; a non-loopback address needs one; without a token requests with an `Origin` header are refused. See `docs/api.md`.
- The control API also has `GET /v1/events?station=ID` (Server-Sent Events: one `--json` event object per message, redacted like `--json`, keep-alive every 15 s, a `note` event when the client lags, at most 16 streams, 503 beyond), `GET /v1/rules`, `PUT`/`DELETE /v1/rules/{action}` (fixed CALLRESULT, like the `respond` script step) and `POST`/`GET`/`DELETE /v1/faults` (like `inject_fault`).
- New dependency: `axum` (HTTP/1 only) with `hyper` and `tower` crates.

### Distribution

- New release target `aarch64-unknown-linux-musl` (static, built on `ubuntu-24.04-arm`) and the npm platform package `@archhaze24/kirby-ocpp-linux-arm64-musl`; the npm launcher prefers it on musl arm64 systems and falls back to it when the glibc package is missing. A release now has 14 files (7 archives + 7 `.sha256`).
- Multi-arch container image `ghcr.io/archhaze24/kirby-ocpp` (`linux/amd64`, `linux/arm64`) built by the release workflow from the static musl binaries; non-root user, state volume `/data`, port 9000, both binaries (`kirby-csms` as the first argument starts the CSMS). `Dockerfile.dev` builds it from a source checkout.
- Homebrew formula and Scoop manifest templates under `packaging/` plus `.github/scripts/render-packaging.sh`; the release workflow renders them with the real checksums into the workflow artifact `packaging`.
- New page `docs/distribution.md`.

### Documentation

- New page `docs/api.md` for the HTTP control API.
- `README.md` is now a landing page; detailed pages live under `docs/` (index in `docs/README.md`): architecture, emulator, CSMS, scenarios and scripts, security, configuration reference, testing, development.

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
  binary per crate (cold test build 3:07 → 1:23, warm test run 64 s → 33 s on the maintainer's
  VM; `cargo nextest` runs the suite in about 15 s).
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
  sanitised before it reaches terminals, logs and reports; C1 controls and bidi controls
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
