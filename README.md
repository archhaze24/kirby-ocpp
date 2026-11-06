# kirby-ocpp

[![CI](https://github.com/archhaze24/kirby-ocpp/actions/workflows/rust.yml/badge.svg?branch=main)](https://github.com/archhaze24/kirby-ocpp/actions/workflows/rust.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/archhaze24/kirby-ocpp/blob/main/LICENSE)

Terminal tools for testing OCPP 1.6J, written in Rust: a charge point emulator and a central system (CSMS) that talk to each other or to your own firmware and backend.
Both validate every payload against the official OCPP 1.6J JSON schemas, speak `ws://`, `wss://` and mutual TLS, and implement the Security Whitepaper (security profiles 1-3). Each has a terminal UI and a headless mode for CI. OCPP 2.0.1 is not supported yet.

**`kirby-ocpp`**, the charge point emulator:

- Terminal UI, or a headless event logger with JSON lines.
- Scenario runner and suite runner for CI, with JSON and JUnit reports.
- Fleet mode: up to 1000 stations in one process.
- Configurable response rules, state kept between runs.

**`kirby-csms`**, the central system:

- Accepts any number of stations, answers with canned replies.
- Sends commands, injects faults, applies response rules.
- Terminal UI, headless mode, JSON-lines commands on stdin, TOML scripts.
- Local HTTP control API for any language or CI.

## Contents

- [Install](#install)
- [Quick start](#quick-start)
- [Documentation](#documentation)
- [Exit codes](#exit-codes)
- [Development](#development)
- [License](#license)

## Install

**npm** (prebuilt binary for your platform, no Rust needed):

```bash
npx kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001
npx kirby-csms --listen 0.0.0.0:9000
npm install -g kirby-ocpp kirby-csms
```

**Cargo** (Rust 1.88 or newer):

```bash
cargo install kirby-ocpp kirby-csms
```

**Release archives**: download from the [GitHub releases](https://github.com/archhaze24/kirby-ocpp/releases). Each archive holds both binaries, the licence, the third-party notices and this README.

| Platform | Target |
| --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu`, static `x86_64-unknown-linux-musl` |
| Linux aarch64 | `aarch64-unknown-linux-gnu`, static `aarch64-unknown-linux-musl` |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple silicon | `aarch64-apple-darwin` |
| Windows x64 | `x86_64-pc-windows-msvc` (`.zip`) |

**From a checkout**: `cargo install --path crates/kirby-ocpp` and `cargo install --path crates/kirby-csms`.

**More channels**: see [docs/distribution.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/distribution.md).

## Quick start

Start a central system:

```bash
kirby-csms --listen 127.0.0.1:9000
```

It opens its terminal UI (the line `listening on ws://127.0.0.1:9000` goes to stderr first). In a second terminal, point an emulator at it:

```bash
kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id CP-001
```

The emulator connects and boots; `CP-001` appears in the CSMS station list. Without a terminal (or with `--no-tui`) both print events to stdout instead.

Headless and CI forms:

```bash
# Headless emulator, JSON lines
kirby-ocpp --url ws://localhost:9000/ocpp --no-tui --json

# One built-in scenario, exit code 0 (pass) or 1 (fail)
kirby-ocpp --url ws://localhost:9000/ocpp run --scenario charge-session

# All built-in scenarios, fresh station each, JUnit report
kirby-ocpp --url ws://localhost:9000/ocpp suite --all-builtins --junit junit.xml

# Fleet of 50 stations
kirby-ocpp --url ws://localhost:9000/ocpp --id 'CP-{n:03}' --count 50 --no-tui

# Scripted CSMS test
kirby-csms --script suite.toml --junit junit.xml --timeout 120
```

`--url` (or `OCPP_URL`) is the only required emulator option. Precedence is flag, then environment variable, then default. Booleans accept `true/false/t/f/yes/no/y/n/on/off/1/0`, case-insensitive; anything else is an error (exit 2). List the built-in scenarios with `kirby-ocpp scenarios list`.

## Documentation

The full index is in [docs/README.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/README.md).

| Topic | Page |
| --- | --- |
| Charge point emulator: TUI keys, headless mode, fleet, state, response rules | [docs/emulator.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/emulator.md) |
| Central system: TUI, stdin commands, rules, fault injection | [docs/csms.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/csms.md) |
| Scenarios, suites and CSMS scripts: variables, loops, JSON and JUnit reports | [docs/scenarios.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/scenarios.md) |
| Local HTTP control API of `kirby-csms` | [docs/api.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/api.md) |
| Security profiles, TLS, mutual TLS, certificates, security events | [docs/security.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/security.md) |
| Every flag and environment variable | [docs/configuration.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/configuration.md) |
| All install channels | [docs/distribution.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/distribution.md) |
| Protocol coverage and deviations from the spec | [docs/OCPP_1.6J_CONFORMANCE.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/OCPP_1.6J_CONFORMANCE.md) |
| Architecture, development loop, tests | [docs/architecture.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md), [docs/development.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/development.md), [docs/testing.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/testing.md) |
| Releasing and history | [RELEASING.md](https://github.com/archhaze24/kirby-ocpp/blob/main/RELEASING.md), [CHANGELOG.md](https://github.com/archhaze24/kirby-ocpp/blob/main/CHANGELOG.md) |

The engines are also published as libraries: `kirby-ocpp-core` (station) and `kirby-csms-core` (CSMS), see the [layer contracts](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md#layer-contracts).

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success (scenario or script passed, normal shutdown) |
| 1 | Scenario or script failed or timed out, or a runtime error |
| 2 | Usage or configuration error |
| 130 | Interrupted by SIGINT during `run` or `--script` |

Details per binary: [docs/configuration.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/configuration.md).

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Workspace crates and conventions: [docs/architecture.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md) and [docs/development.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/development.md).

## License

MIT, see [LICENSE](https://github.com/archhaze24/kirby-ocpp/blob/main/LICENSE). The bundled OCPP JSON schemas belong to the Open Charge Alliance; see [THIRD_PARTY_NOTICES.md](https://github.com/archhaze24/kirby-ocpp/blob/main/crates/kirby-ocpp/THIRD_PARTY_NOTICES.md) or run `kirby-ocpp --licenses`. Crate licences are recorded in `Cargo.lock` and the crates' metadata.
