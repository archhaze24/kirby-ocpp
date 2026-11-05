# Documentation

Index of all pages. The project landing page is the [README](../README.md).

Contents: [Start here](#start-here) · [Pages](#pages)

## Start here

| You are | Read in this order |
| --- | --- |
| A user of the emulator (`kirby-ocpp`) | [README](../README.md) quick start, [Emulator](emulator.md), [Scenarios](scenarios.md), [Configuration](configuration.md) |
| A user of the CSMS (`kirby-csms`) | [README](../README.md), [CSMS](csms.md), [Scenarios](scenarios.md) (scripts), [Security](security.md), [Configuration](configuration.md) |
| Testing TLS, Basic Auth or certificates | [Security](security.md), then [Configuration](configuration.md) |
| A contributor | [Architecture](architecture.md), [Development](development.md), [Testing](testing.md), then the topic page you change |
| A maintainer cutting a release | [Releasing](../RELEASING.md), [Changelog](../CHANGELOG.md) |

## Pages

| Page | Content |
| --- | --- |
| [architecture.md](architecture.md) | Crate map, layer contracts, data flow, invariants, where each concern lives, test layout |
| [emulator.md](emulator.md) | Using the charge point emulator: TUI, headless mode, fleet, state |
| [csms.md](csms.md) | Using the central system: TUI, stdin commands, rules, fault injection, scripts |
| [scenarios.md](scenarios.md) | TOML scenarios and CSMS scripts, built-ins, reports, JUnit |
| [security.md](security.md) | Security profiles 0-3, TLS and mTLS, certificates, security events |
| [configuration.md](configuration.md) | Every flag and environment variable of both binaries, precedence, id templates, `--rule`, exit codes, state files |
| [testing.md](testing.md) | Test layout, mock CSMS, rules for async tests, nextest, Windows trap, CI |
| [development.md](development.md) | Toolchain, commands, conventions, commit style, store version, recipes for adding messages, commands, actions and flags |
| [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md) | Spec coverage and intentional deviations |
| [../RELEASING.md](../RELEASING.md) | Release runbook (tag, GitHub release, crates.io, npm) |
| [../CHANGELOG.md](../CHANGELOG.md) | Release history; the `Unreleased` section collects the next release |
