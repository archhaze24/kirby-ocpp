# kirby-ocpp

Terminal tools for testing OCPP 1.6J, written in Rust:

- **`kirby-ocpp`**: a charge point emulator for local and staging CSMS testing. Terminal UI, headless event logger, scripted scenario runner for CI, fleet mode (up to 1000 stations in one process).
- **`kirby-csms`**: a central system for charge point firmware developers and load tests. Accepts any number of stations, answers with canned replies, sends commands, injects faults. Terminal UI, headless mode, stdin JSON-lines commands, TOML scripts.

Both speak OCPP 1.6 JSON over WebSocket (`ws://`, `wss://`, mutual TLS), validate every payload against the bundled official OCPP 1.6J JSON schemas, and implement the Security Whitepaper (security profiles 1-3, certificates, security events, signed firmware, log upload). OCPP 2.0.1 is not supported yet.

## Contents

- [Install](#install)
- [Quick start](#quick-start)
- [Documentation](#documentation)
- [Exit codes](#exit-codes)
- [Development](#development)
- [License](#license)

## Install

With Node.js (installs the prebuilt binary for your platform, no Rust needed):

```bash
npx kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001
npm install -g kirby-ocpp
npx kirby-csms --listen 0.0.0.0:9000
npm install -g kirby-csms
```

With Cargo (Rust 1.88 or newer):

```bash
cargo install kirby-ocpp
cargo install kirby-csms
```

With Docker (multi-arch, both binaries): `docker run --rm -p 9000:9000 ghcr.io/archhaze24/kirby-ocpp kirby-csms`. Homebrew: `brew install archhaze24/tap/kirby-ocpp`; Scoop: `scoop bucket add kirby https://github.com/archhaze24/scoop-bucket && scoop install kirby-ocpp` (both once the owner has published the tap and bucket). Details: [docs/distribution.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/distribution.md).

Or download a prebuilt archive for Linux (x86_64 and aarch64, glibc or static musl for Alpine and minimal containers), macOS or Windows from the [GitHub releases](https://github.com/archhaze24/kirby-ocpp/releases). Each archive holds both binaries, the licence, the third-party notices and this README. To build from a checkout: `cargo install --path crates/kirby-ocpp` (and `--path crates/kirby-csms`).

## Quick start

Run a central system, then point an emulator at it:

```bash
kirby-csms --listen 127.0.0.1:9000
kirby-ocpp --url ws://127.0.0.1:9000/ocpp --id CP-001            # TUI
```

More forms:

```bash
kirby-ocpp --url ws://localhost:9000/ocpp --no-tui --json         # headless, JSON lines
kirby-ocpp --url ws://localhost:9000/ocpp run --scenario charge-session   # CI scenario, exit code 0/1
kirby-ocpp --url ws://localhost:9000/ocpp suite --all-builtins --junit junit.xml   # many scenarios, fresh station each
kirby-ocpp --url ws://localhost:9000/ocpp --id 'CP-{n:03}' --count 50 --no-tui   # fleet of 50
kirby-csms --script suite.toml --junit junit.xml --timeout 120     # scripted CSMS test
```

`--url` (or `OCPP_URL`) is the only required emulator option. Precedence is flag, then environment variable, then default. Booleans accept `true/false/t/f/yes/no/y/n/on/off/1/0`, case-insensitive; anything else is an error (exit 2).

## Documentation

Start at the [documentation index](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/README.md).

| I want to... | Read |
| --- | --- |
| Use the charge point emulator (TUI keys, headless, fleet, state, response rules) | [docs/emulator.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/emulator.md) |
| Use the central system (TUI, stdin commands, rules, faults) | [docs/csms.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/csms.md) |
| Write scenarios or CSMS scripts, get JSON or JUnit reports | [docs/scenarios.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/scenarios.md) |
| Drive `kirby-csms` over HTTP from any language or CI (local control API) | [docs/api.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/api.md) |
| Test TLS, mTLS, Basic Auth, certificates, security events | [docs/security.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/security.md) |
| Look up a flag or environment variable | [docs/configuration.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/configuration.md) |
| See which parts of the spec are covered and where we deviate | [docs/OCPP_1.6J_CONFORMANCE.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/OCPP_1.6J_CONFORMANCE.md) |
| Understand the code and contribute | [docs/architecture.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md), [docs/development.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/development.md), [docs/testing.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/testing.md) |
| Cut a release | [RELEASING.md](https://github.com/archhaze24/kirby-ocpp/blob/main/RELEASING.md), [CHANGELOG.md](https://github.com/archhaze24/kirby-ocpp/blob/main/CHANGELOG.md) |

Direct jump links:

- Emulator: [TUI](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/emulator.md#terminal-ui), [headless mode](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/emulator.md#headless-mode-and-exit-codes), [fleet mode](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/emulator.md#fleet-mode), [response rules](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/emulator.md#response-rules)
- CSMS: [TUI](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/csms.md#terminal-ui), [stdin commands](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/csms.md#commands-on-stdin), [fault injection](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/csms.md#response-rules-and-fault-injection), [use as a library](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/csms.md#use-as-a-library)
- Library use: `kirby-ocpp-core` (station) and `kirby-csms-core` (CSMS engine) are published crates, see [layer contracts](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md#layer-contracts)

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success (scenario or script passed, normal shutdown) |
| 1 | Scenario or script failed or timed out, or a runtime error |
| 2 | Usage or configuration error |
| 130 | Interrupted by SIGINT during `run` or `--script` |

Details per binary are in [docs/configuration.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/configuration.md).

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Workspace crates and conventions: [docs/architecture.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md) and [docs/development.md](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/development.md).

## License

MIT, see [LICENSE](LICENSE). The bundled OCPP JSON schemas belong to the Open Charge Alliance; see [THIRD_PARTY_NOTICES.md](crates/kirby-ocpp/THIRD_PARTY_NOTICES.md) or run `kirby-ocpp --licenses`. Crate licences are recorded in `Cargo.lock` and the crates' metadata.
