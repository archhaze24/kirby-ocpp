# Testing

How tests are laid out, how to run them and which rules keep them reliable.

Contents: [Quick commands](#quick-commands) · [Layout](#layout) · [Mock CSMS](#mock-csms-kirby-ocpp-testkit) · [Rules](#rules-for-async-and-socket-tests) · [CLI tests](#cli-tests) · [TUI tests](#tui-tests) · [Nextest vs cargo test](#nextest-vs-cargo-test) · [Windows trap](#windows-trap-and-emulation) · [CI](#ci) · [Related pages](#related-pages)

## Quick commands

| Goal | Command |
| --- | --- |
| Full gate (what CI runs) | `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` |
| Fast loop | `cargo nextest run --workspace && cargo test --workspace --doc` |
| One crate | `cargo test -p kirby-ocpp-core` |
| One test binary file | `cargo test -p kirby-ocpp-core --test it station_transactions` |
| Loop a new async test | repeat the run 10 times, for example `for i in $(seq 10); do cargo test -p kirby-ocpp-core --test it <name> || break; done` |

## Layout

* Unit tests sit next to the code in `#[cfg(test)]` modules. Time goes through `Clock` (`ManualClock` in tests); timers use `tokio::time`.
* Integration tests: one binary per crate. `crates/<crate>/tests/it/main.rs` (target `it`) holds one `mod <name>;` per file `tests/it/<name>.rs`, plus shared helper modules declared once there and used as `crate::station_support::...`. One binary instead of one per file keeps link time down (a test binary per file made builds dominated by linking).
* To add a test file: create `tests/it/<name>.rs` and add `mod <name>;` to `tests/it/main.rs`.
* `kirby-csms` is the exception: a single `crates/kirby-csms/tests/cli.rs`.
* Tests of one crate share one process under `cargo test`: no globals, no cwd changes, no environment changes.

| Crate | Integration tests | Helpers |
| --- | --- | --- |
| `kirby-ocpp-core` | `tests/it/`: `station_*.rs` (lifecycle, transactions, power, sampling, security, TLS, persistence, response rules, ...), `ws_*.rs` (connection, calls, TLS), `scenario_runner.rs`, `fleet.rs` | `station_support/`, `ws_support/` |
| `kirby-csms-core` | `tests/it/`: `engine.rs`, `security.rs`, `state.rs`, `redaction.rs` | `common/` where present |
| `kirby-ocpp-tui` | `tests/it/`: `model_flow.rs`, `fleet_flow.rs` | |
| `kirby-ocpp` | `tests/it/`: `cli.rs`, `fleet.rs`, `headless.rs`, `hardening.rs` | |
| `kirby-csms` | `tests/cli.rs` | |
| `kirby-ocpp-testkit` | `tests/it/` (tests of the mock itself) | |

## Mock CSMS (`kirby-ocpp-testkit`)

Dev-only crate (`publish = false`). `MockCsms::start(CsmsOptions)` runs a WebSocket server on `127.0.0.1` with an ephemeral port, `ws://` or `wss://` (including mTLS and subprotocol negotiation). It reuses canned replies and faults from `kirby-csms-core`.

* `respond(action, closure)` overrides the answer of one action.
* `inject(Fault::...)` scripts failures for the next N occurrences: CALLERROR, silence, socket close, invalid payload.
* `send_call(action, payload)` sends a CSMS-originated CALL to the station.
* `wait_for_call`, `wait_for_call_after(mark, action, predicate, timeout)` and `mark()` wait on recorded CALLs; `url()` is what the station connects to.
* All waits are event driven and take a timeout.

## Rules for async and socket tests

* Each test owns its station, its mock CSMS and its `tempfile` directory. Nothing is shared between tests.
* No fixed sleeps. Wait on a snapshot (`watch_snapshot`), on events (`subscribe`) or on `wait_for_call`.
* Never `tokio::time::pause` together with real sockets; socket tests use short real timeouts.
* A responder closure that blocks a runtime worker hangs the whole runtime; wrap a blocking wait in `tokio::task::block_in_place`.
* Loop every new or changed async test about 10 times before trusting it.
* Do not rely on `unwrap` on external data in production code; tests may `unwrap`/`expect`.

## CLI tests

Environment variables cannot be set inside a test (edition 2024 makes `std::env::set_var` unsafe, and `unsafe` is forbidden). CLI tests therefore spawn the real binary via `env!("CARGO_BIN_EXE_kirby-ocpp")` or `env!("CARGO_BIN_EXE_kirby-csms")` and set the environment on the child `Command`. They check exit codes (0, 1, 2), stdout and report files, and use temp directories for `--state-dir` and `--log-file`.

## TUI tests

* The UI is `update(&mut App, Msg) -> Vec<Effect>`: test it as a pure function, no terminal needed.
* Rendering uses `ratatui::backend::TestBackend`: draw a frame and assert on the buffer text.
* Key handling (`keys.rs`) is tested by feeding key events to `update`.
* Terminal-guard, clipboard and pty behavior is only partly covered; manual checks on real terminals remain part of a release.

## Nextest vs cargo test

* `cargo nextest run` runs every test of every binary in parallel across processes: about 15 s against about 60 s for `cargo test` on a warm target directory. It does not run doctests, so add `cargo test --workspace --doc`.
* CI and the final gate before a push use plain `cargo test --workspace`. A test that only passes under nextest (process per test) but fails under `cargo test` (shared process) breaks the "no globals" rule; fix the test.
* Use a separate `CARGO_TARGET_DIR` per parallel checkout and set `CARGO_INCREMENTAL=0` to keep build directories small.

## Windows trap and emulation

CI builds with `RUSTFLAGS=-D warnings` on Windows. Code behind `#[cfg(unix)]` leaves helpers, imports or constants unused there, and the warning becomes an error. Emulate it on Linux or macOS before pushing anything that touches `cfg(unix)`:

```bash
rsync -a --exclude .git --exclude target ./ /path/to/win-copy/
cd /path/to/win-copy
grep -rl 'cfg' crates --include='*.rs' | xargs sed -i \
  -e 's/#\[cfg(unix)\]/#[cfg(any())]/g' \
  -e 's/#\[cfg(not(unix))\]/#[cfg(not(any()))]/g' \
  -e 's/cfg!(unix)/cfg!(any())/g'
RUSTFLAGS='-D warnings' cargo clippy --workspace --all-targets -- -D warnings
```

`cfg(any())` is always false, so the Unix branch disappears exactly as on Windows. Use a separate `CARGO_TARGET_DIR` for the copy. Also remember that paths, line endings and file permissions differ on Windows.

## CI

`.github/workflows/rust.yml` runs on pushes to `main`, `ci-check/**` and `dev-*`, on pull requests and manually. One matrix job named by OS (`ubuntu-latest`, `macos-latest`, `windows-latest`, `fail-fast: false`, 30 min timeout), with `RUSTFLAGS=-D warnings`:

1. `git config --global core.autocrlf false` (keep line endings as committed).
2. Checkout, stable Rust with `clippy` and `rustfmt`, `Swatinem/rust-cache`.
3. `cargo fmt --all --check`
4. `cargo clippy --workspace --all-targets -- -D warnings`
5. `cargo test --workspace`

`.github/workflows/release.yml` builds and publishes release artifacts; see [RELEASING.md](../RELEASING.md).

## Related pages

[Development](development.md) · [Architecture](architecture.md#tests) · [Docs index](README.md)
