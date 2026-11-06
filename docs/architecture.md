# Architecture

This page is self-contained: read it first, then open only the topic page or source directory you need. It describes the code as of 0.5.0-dev (OCPP 1.6J plus the Security Whitepaper).

Contents: [Crate map](#crate-map) · [Layer contracts](#layer-contracts) · [Emulator internals](#emulator-internals) · [CSMS internals](#csms-internals) · [Data flow](#data-flow) · [Invariants](#invariants) · [Where each concern lives](#where-each-concern-lives) · [Tests](#tests) · [Related pages](#related-pages)

## Crate map

```text
kirby-ocpp (bin)   ---> kirby-ocpp-tui ---> kirby-ocpp-core
kirby-csms (bin)   ---> kirby-csms-tui ---> kirby-csms-core ---> kirby-ocpp-core
                        kirby-csms-tui ---> kirby-ocpp-tui   (shared terminal pieces only)
kirby-ocpp-testkit (dev only, publish = false) ---> kirby-csms-core, kirby-ocpp-core
```

Dependency rule: UI crates depend on core crates, never the reverse. `kirby-csms-core` reuses the OCPP types, schemas and WebSocket framing of `kirby-ocpp-core`.

| Crate | Role | Published |
| --- | --- | --- |
| `kirby-ocpp-core` | UI-free charge point emulator and the OCPP layer (see below) | yes |
| `kirby-ocpp-tui` | Emulator terminal UI (ratatui). Also holds generic pieces reused by the CSMS UI: terminal guard, `logview`, `text`, `clipboard`, `palette` | yes |
| `kirby-ocpp` | Emulator binary: `clap` CLI, headless runner, fleet mode, `run`/`scenarios` subcommands | yes |
| `kirby-csms-core` | Multi-station CSMS engine: `CsmsServer` / `CsmsHandle` | yes |
| `kirby-csms-tui` | CSMS terminal UI: station list, details, log, command forms, rules | yes |
| `kirby-csms` | CSMS binary: TUI on a TTY, otherwise headless (JSON lines, stdin commands, TOML scripts) | yes |
| `kirby-ocpp-testkit` | Scripted mock CSMS for integration tests (plain and TLS); reuses canned replies and faults from `kirby-csms-core` | no |

### `kirby-ocpp-core` modules (`crates/kirby-ocpp-core/src`)

| Module | Content |
| --- | --- |
| `ocpp/` | `Frame` (CALL / CALLRESULT / CALLERROR), the `Action` trait, 39 typed messages in `messages/` (OCPP 1.6 plus the Security Whitepaper set), the embedded JSON-schema registry (`schema.rs`; schema files in `crates/kirby-ocpp-core/schemas/`), `Responder`, shared types (`types.rs`), `redact_secrets` |
| `ws/` | One connection task per link: handshake (`handshake.rs`, subprotocol, Basic Auth), rustls TLS incl. mTLS (`tls.rs`), serialized CALL queue, ping watchdog (`task.rs`) |
| `station/` | The single `StationActor` plus `handle.rs` (public handle), `command.rs`, `snapshot.rs`, `event.rs`; `handlers/` has one file per group of CSMS-originated CALLs; pure logic modules: `smart_charging.rs`, `charging_profiles.rs`, `power.rs`, `sampling.rs`, `config_registry.rs`, `local_auth_list.rs`, `reservation.rs`, `authorization.rs`, `certificates.rs`, `security.rs`, `secure.rs`, `maintenance.rs`, `response_rules.rs`, `status.rs` |
| `scenario/` | TOML scenario model (`model.rs`), runner (`runner.rs`), variables and capture (`vars.rs`, shared with the `kirby-csms` scripts), JUnit renderer (`junit.rs`), nine built-ins (`builtin.rs`, TOML in `crates/kirby-ocpp-core/scenarios/`) |
| `fleet.rs` | `Fleet`: N independent stations, id templates, merged event stream |
| `store.rs` | `StationStore`: atomic versioned JSON state |
| `config.rs`, `ids.rs`, `clock.rs`, `error.rs` | `StationConfig` + validation, id newtypes, `Clock` (`SystemClock`, `ManualClock`), error types |

### Binaries

* `kirby-ocpp/src`: `cli.rs` (flags and env, CLI > env > default, strict booleans), `main.rs` (config, spawn, choose UI or headless, the `run` scenario runner, reports, exit codes), `headless.rs` (event printer, stdout and log-file helpers), `fleet.rs` (`--count`, fleet runs), `suite.rs` (`kirby-ocpp suite`: sources, fresh station per scenario, summary report).
* `kirby-csms/src`: `cli.rs`, `main.rs`, `headless.rs` (event printer, stdout/log-file helper threads), `stdin_cmd.rs` (JSON-lines commands), `commands.rs` (call execution, result objects and the in-flight limit shared by stdin and the API), `api.rs` (HTTP control API on `axum`), `script.rs` (TOML scripts, report, JUnit).

## Layer contracts

UIs and runners never touch internals; they use these surfaces only.

**Emulator** (`kirby_ocpp_core::station::StationHandle`):

| Call | Meaning |
| --- | --- |
| `send(Command) -> Result<Outcome, StationError>` | The one way to act. `Command` is plain serde data (no reply channel). Typed helpers (`plug_in`, `authorize`, ...) are sugar over it. A CSMS rejection of an idTag is an `Outcome`, not an error. |
| `snapshot()` / `watch_snapshot()` | Immutable serde read model; latest value wins. |
| `subscribe()` | Lossy `broadcast` of `Event::{Frame, Log, StateChanged}`. `Frame` carries a `FrameRecord{ts, direction, msg_id, action, frame}` (JSONL export is trivial). `StateChanged` is only a nudge; the data is in the snapshot. |
| `config()` | Static launch config (URL, vendor/model, initial connector); not in the snapshot. |

The station does not connect by itself: the caller sends `Command::Connect`.

**CSMS** (`kirby_csms_core::CsmsHandle`, from `CsmsServer::start(CsmsConfig)`):

| Call | Meaning |
| --- | --- |
| `call`, `call_timeout`, `call_typed::<A>` and ~26 typed wrappers (`reset`, `remote_start_transaction`, ...) | CSMS-originated CALLs to a station id. `call_typed` validates request and response against the embedded schema. |
| `state(id)` | `StationState`: per-station protocol read model (boot info, connectors, transactions with meter readings, security events, log/firmware status, last CSR). Pull model plus `CsmsEvent::StateChanged{station}`. |
| `stations()`, `station(id)`, `history(id)` | Registry and bounded frame history (`FrameEntry`). |
| `subscribe()` | `CsmsEvent` stream: `Connected`, `Disconnected` (with `DisconnectReason`), `Frame`, `StateChanged`, `AuthFailed`, `Malformed`, `Forgotten`. |
| `respond(action, f)`, `clear_responder`, `inject(Fault)`, `clear_faults`, `faults_remaining`, `responder_actions` | Response rules and fault injection (global across stations). |
| `forget(id)`, `security_profile()`, `local_addr()`, `url()`, `shutdown()` | Housekeeping. |

## Emulator internals

**Actor model.** One `StationActor` task owns all station state (connectors, config registry, authorization list and cache, charging profiles, reservations, offline queue, certificates). Nothing is shared behind a lock.

```text
 UI / headless / scenario            StationActor (owns state)                   ws task (owns socket)
 handle.send(Command) --mpsc-->      match command, mutate state  --calls-->     CALL queue -> socket
        <--oneshot Outcome--         publish() after every change <--mpsc--      ConnectionEvent: Frame / Call / Closed
 handle.snapshot()    <--watch--
 handle.subscribe()   <--broadcast-- Event(Frame | Log | StateChanged)
 timers (tokio::time) -> internal event channel: heartbeat, reconnect, boot retry, status debounce, persist flush, meter sampling, reservation expiry, maintenance, log upload, signed firmware
```

* The loop is a `select!` over the command queue, the current connection's events, one internal channel and the timer wheel (`TimerWheel<TimerKey>`: keyed, re-armable, cancellable, clamped to 24 h). The snapshot is published once per iteration if it differs from the last one.
* The actor never blocks on I/O. Connect, CALLs and state writes run as spawned tasks and report back as internal messages. An outbound CALL is a spawned future plus a boxed continuation `FnOnce(&mut StationActor, Result<Value, CallError>)` keyed by a token; the continuation runs exactly once (on completion, or from `fail_pending_calls` when the link is lost, disconnected or shut down).
* The actor ends on `Command::Shutdown` or when every handle is dropped. Timers and the ws task therefore never hold a clone of the command sender; they feed an internal channel instead.
* **Registration gate:** until BootNotification is `Accepted` only the boot may be sent; while `Pending` also CSMS-triggered messages; `SendRaw` is never gated. A failure is `CallError::NotRegistered`.
* **CALL order:** `call_raw` queues the first attempt on the connection synchronously (`Connection::enqueue`, which only sends on the connection task's unbounded command channel) and then spawns the task that waits for the answer. The order in which actor code issues CALLs is therefore the order on the wire; spawning the whole `call()` would leave it to the scheduler (multi-thread runtime).
  * Retries (`StartTransaction`, `StopTransaction`, `MeterValues`) are queued again after the retry interval, behind any CALL made since, so they may be reordered relative to later CALLs (a retried `StartTransaction` can follow a later one).
  * The one exception is a retried `MeterValues` sample: once the `StopTransaction` of its transaction is queued the retry is dropped (Info log), so the CSMS never gets a sample after the stop.
  * A request that cannot be queued (schema or serialization error, closed link) is not retried.
  * The connection task prunes abandoned queued CALLs only when its queue has doubled past 64 entries (amortized cost); a withdrawn CALL is still skipped at pump time. Dropping the waiter still withdraws a CALL that is queued.
* **Generations:** the link generation is bumped on every connect attempt and teardown; results of an older generation are ignored.
* **Inbound dispatch:** `handlers::dispatch_incoming` maps an action name to a typed `fn(&mut StationActor, Responder<A>, A::Req)`. Decode failures become `TypeConstraintViolation`. Handlers reply first and then start follow-up work. Response rules (`response_rules.rs`) are applied before payload decoding.
* **Ids:** `ConnectorId(u32)` (0 = the charge point), `ReservationId(i32)`, `IdTag` (validated, at most 20 chars); "no id" is `Option`, never `0`. `TxId` is `Csms(i32) | Local(u64)`: a transaction started offline carries `Local` until `StartTransaction.conf`; `TxId::wire()` is `None` for `Local`. Transaction state per connector: `Idle | PendingStart | Active | Stopping`.
* **Metering:** a connector draws `min(nominal, profile limit)` while `Charging`; energy is the integral of power over monotonic time, settled at the top of each actor iteration and flushed into the persisted `meter_wh` where it is read.
* **Offline samples:** transaction `MeterValues` taken offline wait in `SampleQueue` (global cap 1000, no per-transaction share; persisted, so the state file is rewritten (debounced 200 ms) while they queue) and are flushed in call order before the transaction's `StopTransaction`. Restore keeps a sample only while a connector runs its transaction or the offline queue has an entry for it. In-flight security events count towards the 100-event cap (snapshot `pending_security_events` never exceeds 100).
* **Offline:** failed or offline Start/Stop are queued (`offline.rs`) and synced after the next accepted boot, one chain per entry; the status re-send follows the sync.
* **Persistence:** `StationStore` writes `<state dir>/<escaped id>.station.json` (temp file, fsync, rename, `0600`; directory fsync on unix). Secrets (`AuthorizationKey`, certificate private key, pending CSR key) live in the separate `<id>.station.secrets.json`. Corrupt or foreign files are moved to `.bak`; a newer file version is refused untouched. This holds for the secrets file too: one that is unreadable or written by a newer build is never overwritten, and secrets are not persisted during that run. Writes are debounced (200 ms) or immediate for important changes, off the actor task.

## CSMS internals

* `CsmsServer` accepts any number of stations on `ws(s)://host:port/<any prefix>/<station id>`; the id is the last non-empty percent-decoded path segment. Ids with control, line-separator or bidi characters (`kirby_ocpp_core::text::is_unsafe_char`) or over 128 bytes are refused with HTTP 400. A second connection with the same id replaces the first (`Disconnected{Replaced}`).
* Outbound CALLs are serialized per station (one outstanding); at most 1024 more wait behind it, further `call()`s fail with `CsmsError::QueueFull` at once (a slot is reserved when `call()` is made and freed when the call leaves the queue; callers that gave up are pruned); the timeout counts from the `call()`, queue time included; when a caller drops its call after the write, the session waits at most the default call timeout more for the station before the next CALL is written; a late reply after a timeout is recorded but ignored.
* Inbound CALLs: schema-validated if `validate_inbound`; then a `Fault` (global, counted), a responder, or the canned `default_reply`; known non-CP actions get `NotSupported`, unknown `NotImplemented`; malformed frames with a readable id get `FormationViolation`.
* State tracking (`state.rs`) is built from the actual reply payload of CALLs answered with `Reply::Result`. Stopped transactions are capped (`transaction_history_cap`), active ones at 256. Free text from stations (boot fields, connector `info`/vendor codes, security events) is cut to the schema maximum (255 characters at most).
* Security profiles 0-3 (`auth.rs`, `tls.rs`): Basic Auth checked in the handshake callback (constant-time, user must equal the URL id), TLS, mTLS with client CN equal to the station id; refusals emit `CsmsEvent::AuthFailed`.
* One global `std::sync::Mutex` guards engine state; events are emitted under the lock to keep ordering.
* The `kirby-csms` binary can expose `CsmsHandle` over a local HTTP API (`api.rs`, `axum` with only the `http1` and `tokio` features). It only uses the handle and `commands.rs`, which the stdin handler shares, so result shapes and the 256-command in-flight limit are the same for both. The SSE stream (`/v1/events`, at most 16) uses the `--json` renderer of `headless.rs` (`Renderer`), and fault parsing/validation is shared with the script steps. See [api.md](api.md).
* Limits: frame size 1 MiB, 100 000 stations, 1024 sockets in their TLS/WebSocket handshake at once (`max_pending_handshakes`; extra sockets are closed), per-action counters capped at 64 keys, write timeout 10 s.

## Data flow

1. **Emulator, interactive:** key press -> `kirby-ocpp-tui` `Msg` -> pure `update(&mut App, Msg) -> Vec<Effect>` -> effect executor calls `StationHandle::send` -> actor mutates state / sends a CALL -> snapshot + `Event` back -> UI redraw.
2. **Emulator, scenario:** `scenario::run_scenario` (or `ScenarioRunner::run` in the TUI) drives the same `StationHandle`, waits on events and snapshots, produces a report (PASS/FAIL/SKIP per step) that the binary prints and optionally writes as JSON and JUnit.
3. **CSMS-originated command to a station:** TUI form / stdin line / script step -> `CsmsHandle::call` (or typed wrapper) -> station connection -> reply -> result in the UI status line, stdin answer line or script report. State changes arrive through `StateChanged`.
4. **Frames:** every frame in either direction is recorded (`FrameRecord` / `FrameEntry`) after `redact_secrets` (unless `--show-secrets`).

## Invariants

* **One reply per inbound CALL.** The `Responder<A>` takes `self` on every reply method, so the compiler enforces at most one reply; if a handler drops it, `Drop` sends CALLERROR `InternalError` (best effort). `ReplyHandle::abandon` is the explicit no-reply path (`ignore` / `drop` rules).
* **Outbound CALLs are serialized**: the next CALL goes out only after the current one resolved (result, error or timeout). `ConnectOptions::serialize_calls = false` exists for negative tests. A dropped `call()` future withdraws a not yet written CALL.
* **Schema validation both ways** with the embedded OCPP 1.6J schemas; violations map to `OccurenceConstraintViolation` / `TypeConstraintViolation` / `PropertyConstraintViolation` / `FormationViolation`.
* **State file format changes only with a `STORE_VERSION` bump** (now 9, `store.rs`). Older files load with serde defaults; newer ones are refused. No compatibility with the old TypeScript state files.
* **Redaction:** `AuthorizationKey` values are replaced by `***` in recorded frames, logs and the config log (`ocpp::frame::redact_secrets`) unless `--show-secrets`. Secrets never go into the main state file. Station ids and payloads are attacker-controlled: sanitize before printing (control characters become `·`).
* **No `unsafe`; no `unwrap`/`expect`/indexing on external data** (clippy pedantic is on).
* **Bounded memory** everywhere external input grows: log buffers, frame history, scenario CALL backlog (10 000 / ~16 MiB), stdin lines (1 MiB), stdout/log-file helper threads drop lines instead of blocking. The one known exception: CSMS station records are never evicted (`forget()` only), bounded per station and by `max_stations` (see [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md)).
* **Spec wins over TS parity.** Intentional deviations are listed in [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md).
* Keep public shapes (docs, scenarios, the HTTP API) free of 1.6J-only assumptions where it is cheap. OCPP 2.0.1 and 2.1 are not supported in 0.5.

## Where each concern lives

| Concern | Place |
| --- | --- |
| CLI flags, env, exit codes (emulator) | `crates/kirby-ocpp/src/cli.rs`, `main.rs`, `headless.rs` |
| CLI flags, env, exit codes (CSMS) | `crates/kirby-csms/src/cli.rs`, `main.rs`, `headless.rs` |
| Add or change an OCPP message | `kirby-ocpp-core/src/ocpp/messages/`, schema file in `crates/kirby-ocpp-core/schemas/`, register in `ocpp/schema.rs` |
| Handle a CSMS command in the emulator | `kirby-ocpp-core/src/station/handlers/` |
| Emulator user command | `station/command.rs` (type), `station/commands.rs` (execution), TUI action in `kirby-ocpp-tui/src/actions.rs` |
| Station state shown to UIs | `station/snapshot.rs`, `station/views.rs` |
| Smart charging, metering, power | `smart_charging.rs`, `charging_profiles.rs`, `power.rs`, `sampling.rs` |
| Configuration keys | `station/config_registry.rs` |
| Response rules (emulator) | `station/response_rules.rs`, CLI `--rule` |
| Certificates, security events, profiles (emulator) | `certificates.rs`, `security.rs`, `secure.rs`, `ws/tls.rs`, `ws/handshake.rs` |
| Scenarios, variables, loops and reports | `kirby-ocpp-core/src/scenario/` (`vars.rs` for `${name}` and capture) |
| Suite runs (several scenarios, summary, JUnit) | `kirby-ocpp/src/suite.rs` |
| Fleet mode | `kirby-ocpp-core/src/fleet.rs`, `kirby-ocpp-tui/src/fleet.rs`, `kirby-ocpp/src/fleet.rs` |
| Persistence | `kirby-ocpp-core/src/store.rs`, `station/persistence.rs` |
| CSMS engine, typed commands, faults, auth | `kirby-csms-core/src/` (`server.rs`, `typed.rs`, `fault.rs`, `auth.rs`, `tls.rs`, `state.rs`) |
| CSMS script and stdin commands | `kirby-csms/src/script.rs`, `stdin_cmd.rs`, shared `commands.rs` |
| CSMS HTTP control API (routes, auth, limits) | `kirby-csms/src/api.rs`, [api.md](api.md) |
| TUI forms, keys, palette | `kirby-ocpp-tui/src/{forms,keys,palette}.rs`, `kirby-csms-tui/src/{form,keys,actions}.rs` |
| Release plumbing | `RELEASING.md`, `.github/workflows/`, `.github/scripts/release-notes.sh`, `npm/` |

## Tests

* Pure logic: unit tests next to the code. Time goes through `Clock`; timers use `tokio::time`. Never `tokio::time::pause` together with real sockets; socket tests use short real timeouts.
* Integration tests: **one binary per crate**, `crates/<crate>/tests/it/main.rs` (target `it`) with one `mod <name>;` per file `tests/it/<name>.rs` plus shared helper modules (`station_support/`, `ws_support/`, `common/`) used as `crate::station_support::...`. To add a test file: create it and add `mod <name>;` to `main.rs`. `kirby-csms` has a single `tests/cli.rs`.
* Each scenario owns its station, mock CSMS (`kirby-ocpp-testkit`) and `tempfile` dir. No fixed sleeps: wait on snapshots, events or `wait_for_call`. Tests share one process under `cargo test`: no globals, cwd or env changes (env vars cannot be set in edition 2024; CLI tests spawn the binary with `CARGO_BIN_EXE_*`).
* TUI: `ratatui::backend::TestBackend` snapshot tests and pure `update` tests.
* Done criterion: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`. CI runs it on ubuntu, macos and windows with `RUSTFLAGS=-D warnings`.

## Related pages

* [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md): protocol coverage and deviations.
* [RELEASING.md](../RELEASING.md): release runbook.
* [CHANGELOG.md](../CHANGELOG.md): per-version changes.
* [README.md](../README.md): user-facing overview and option tables.
