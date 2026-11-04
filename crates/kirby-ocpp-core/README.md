# kirby-ocpp-core

UI-free OCPP 1.6J charge point emulator core. It is the engine behind the
[`kirby-ocpp`](https://crates.io/crates/kirby-ocpp) terminal tool.

- Typed OCPP 1.6J frames and 39 messages (core profiles plus the Security Whitepaper set), checked against the embedded official JSON schemas.
- A single `StationActor` that owns all station state: transactions, metering, smart charging, local list, reservations, configuration, certificates and security events.
- A WebSocket connection layer: rustls TLS including mTLS, a serialized CALL queue and a ping watchdog.
- TOML scenarios with a runner and a JUnit renderer, a fleet runner and an atomic versioned state store.

A UI or test talks to the station only through `StationHandle`: it sends typed commands, reads snapshots and subscribes to events.

API documentation: <https://docs.rs/kirby-ocpp-core>. Project home, CLI usage and the conformance notes: <https://github.com/archhaze24/kirby-ocpp>.

## Licence

MIT. The bundled OCPP JSON schemas belong to the Open Charge Alliance and are used under CC BY-ND 4.0, see `schemas/README.md`.
