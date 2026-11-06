# Development

Toolchain, commands, conventions and recipes for changing the code. Read [Architecture](architecture.md) first for the crate map and contracts.

Contents: [Toolchain](#toolchain) · [Build and test](#build-and-test) · [Definition of done](#definition-of-done) · [Code conventions](#code-conventions) · [Commits](#commits) · [Store version](#store-version) · [Recipes](#recipes) · [Keeping docs in step with code](#keeping-docs-in-step-with-code) · [Related pages](#related-pages)

## Toolchain

* Rust edition 2024. MSRV: `rust-version = "1.88"` in `Cargo.toml`; do not use newer language or std features without raising it.
* `rust-toolchain.toml` pins `stable` with `clippy` and `rustfmt`.
* Workspace: 7 crates under `crates/` (see [Architecture](architecture.md#crate-map)); `kirby-ocpp-testkit` is not in `default-members` and is not published.

## Build and test

```
cargo build --workspace
cargo run -p kirby-ocpp -- --url ws://localhost:9000/ocpp
cargo run -p kirby-csms -- --listen 127.0.0.1:9000
cargo nextest run --workspace && cargo test --workspace --doc   # fast loop
cargo test --workspace                                          # what CI runs
```

* The dev profile uses `debug = "line-tables-only"`; test binaries are linked once per crate, which keeps builds quick.
* Set `CARGO_TARGET_DIR` to a directory outside the source tree if you build several checkouts, and `CARGO_INCREMENTAL=0` to save disk.
* Details of test layout and rules: [Testing](testing.md).

## Definition of done

```
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

CI runs this on Linux, macOS and Windows with `RUSTFLAGS=-D warnings`. Before pushing, also run the [Windows emulation](testing.md#windows-trap-and-emulation) if you touched `cfg(unix)` code. New or changed async tests must be looped several times.

## Code conventions

* `unsafe` is forbidden (`unsafe_code = "forbid"`). `todo!` and `dbg!` are denied. `unused_must_use` is denied.
* Clippy `pedantic` is on (warn, so `-D warnings` fails on it). Opt-outs live in the workspace `[workspace.lints.clippy]`; do not add `#[allow]` without a reason in a comment.
* No `unwrap`, `expect` or direct indexing on external data (frames, payloads, files, CLI input). Station ids and payloads are attacker-controlled: sanitise before printing to a terminal or file name (escape control characters, escape ids for file names).
* Secrets: `AuthorizationKey` values are redacted in logs and history unless `--show-secrets`. New secret-bearing fields must go through `redact_secrets` and be kept out of the main state file (`<id>.station.secrets.json`).
* Spec behavior wins over parity with the old TypeScript tool. Intentional deviations are listed in [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md).
* Outbound CALLs are serialized; every inbound CALL gets exactly one reply (`Responder` is consume-on-use). See [Invariants](architecture.md#invariants).
* UIs and runners use `StationHandle` / `CsmsHandle` only, never station internals. UI crates depend on core crates, never the reverse.
* Keep public shapes (docs, scenario engine, control surfaces) free of 1.6J-only assumptions where that is cheap.

## Commits

Short English messages in the style of `git log`, one layer per commit:

| Prefix | Use |
| --- | --- |
| `feat:` | New behavior |
| `fix:` | Bug fix |
| `docs:` | Documentation only |
| `refactor:` | No behavior change |
| `ci:` | Workflows |
| `chore:` | Versions, tooling, housekeeping |
| `test:` | Tests only |
| `release:` | Release commit (maintainer) |

User-visible changes also get a line under `## [Unreleased]` in [CHANGELOG.md](../CHANGELOG.md); release notes are taken from it. Tags, GitHub releases, crates.io and npm publishing are done by the maintainer: [RELEASING.md](../RELEASING.md).

## Store version

The station state file is versioned: `STORE_VERSION` in `crates/kirby-ocpp-core/src/store.rs` (now 8). Any change to the persisted format needs a bump of that constant and a CHANGELOG note; new fields get `#[serde(default)]` so older files still load (there is no migration step); a file with a newer version is refused untouched. Files that are corrupt or belong to another charge point are moved to `.bak`. Add a persistence test in `tests/it/station_persistence.rs`.

## Recipes

Start from the table in [Architecture](architecture.md#where-each-concern-lives). Each recipe lists files in order.

### Add an OCPP message

1. Add the JSON schema file(s) to `crates/kirby-ocpp-core/schemas/` and register them in `ocpp/schema.rs`.
2. Add the request/response types in `crates/kirby-ocpp-core/src/ocpp/messages/` and implement `Action` (name, request, response); export from the module.
3. CSMS-originated command the station handles: add a handler in `station/handlers/` and route it; honor `response_rules.rs` (the rule action list) and the config keys it depends on (`config_registry.rs`).
4. Station-originated call: add a `Command` (below) and send it through the serialized CALL queue.
5. CSMS side: add the typed command in `kirby-csms-core/src/typed.rs` and its canned reply (also used by the testkit).
6. Tests: handler test in `kirby-ocpp-core/tests/it/station_*.rs`, schema round trip, CSMS test in `kirby-csms-core/tests/it/engine.rs`.
7. Docs: message counts in [Architecture](architecture.md), [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md), [CHANGELOG.md](../CHANGELOG.md).

### Add an emulator command

1. Add a variant to `Command` in `station/command.rs` (plain serde data) and, if useful, a typed helper on `StationHandle`.
2. Execute it in `station/commands.rs`; put pure logic in its own module with unit tests.
3. If the UI needs new state, add it to `station/snapshot.rs` (and `views.rs`); bump `STORE_VERSION` if it is persisted.
4. Add a scenario action in `scenario/model.rs` / `runner.rs` if scripts should reach it.
5. Test in `kirby-ocpp-core/tests/it/`.

### Add a TUI action

1. Define the action and its availability in `kirby-ocpp-tui/src/actions.rs`; the command palette picks it up from there.
2. If it needs input, add a form in `forms.rs`; add a key in `keys.rs` if it has an accelerator.
3. Handle the `Msg` in the pure `update` and return an `Effect` that sends the `Command`.
4. Add `update` and `TestBackend` tests; update the keys table in [Emulator](emulator.md). For the CSMS UI the files are `kirby-csms-tui/src/{form,keys,actions}.rs` and [CSMS](csms.md).

### Add a CLI flag

1. Add the field in `crates/kirby-ocpp/src/cli.rs` (or `crates/kirby-csms/src/cli.rs`) with `long`, `env = "..."`, `value_name`, a `value_parser` that enforces the range (strict `BoolishValueParser` for booleans) and a doc comment (it becomes the `--help` text).
2. Apply it in `main.rs` / the config builder (`StationConfig` validation in `kirby-ocpp-core/src/config.rs` or `CsmsConfig`).
3. Env names: emulator `OCPP_*` for connection options and `KIRBY_OCPP_*` for tool options; CSMS always `KIRBY_CSMS_*`.
4. Add a CLI test that spawns the binary: default, env, flag over env, invalid value (exit 2).
5. Update [Configuration](configuration.md), the README options table if the flag is there, and CHANGELOG.

## Keeping docs in step with code

* Flag and environment tables in [Configuration](configuration.md) and the README must match `--help` exactly (flag, env name, default, range). After changing a `cli.rs`, build the binaries and diff `kirby-ocpp --help`, `kirby-ocpp run --help`, `kirby-ocpp scenarios --help` and `kirby-csms --help` against the tables.
* Counts in docs (messages, built-in scenarios, tabs, keys) change with the code: grep for the old number.
* Behavior changes go into [CHANGELOG.md](../CHANGELOG.md) and, if they are deviations from the spec, [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md).
* Structural changes (new crate, new module, moved contract) update [Architecture](architecture.md) in the same commit.
* Docs use relative links; new pages are added to the [docs index](README.md).

## Related pages

[Architecture](architecture.md) · [Testing](testing.md) · [Configuration](configuration.md) · [Releasing](../RELEASING.md) · [Docs index](README.md)
