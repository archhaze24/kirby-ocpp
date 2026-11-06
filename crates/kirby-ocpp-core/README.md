# kirby-ocpp-core

UI-free OCPP 1.6J charge point emulator core. It is the engine behind the [`kirby-ocpp`](https://crates.io/crates/kirby-ocpp) terminal tool and the base of the whole workspace: `kirby-csms-core` reuses its frames, schemas and WebSocket layer.

**Most users want the binaries**, not this library:

```
cargo install kirby-ocpp kirby-csms    # or: npm install -g kirby-ocpp kirby-csms
```

## What it is

- Typed OCPP 1.6J frames and 39 messages (core profiles plus the Security Whitepaper set), checked against the embedded official JSON schemas.
- A single `StationActor` that owns all station state: transactions, metering, smart charging, local list, reservations, configuration, certificates, security events.
- A WebSocket connection layer: rustls TLS including mTLS, a serialized CALL queue, a ping watchdog.
- TOML scenarios with a runner and JUnit renderer, a fleet runner (`fleet::Fleet`) and an atomic versioned state store.

It is not a UI, not a CLI and not a central system (see `kirby-csms-core`). It has no ratatui or clap dependency.

## Entry points

`Station::spawn` returns a `StationHandle`. The handle is the only way in: `send(Command)`, `snapshot()` / `watch_snapshot()`, `subscribe()` (`Event`).

```rust,no_run
use std::sync::Arc;
use kirby_ocpp_core::station::{Command, Station};
use kirby_ocpp_core::{StationConfig, SystemClock};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let config = StationConfig::new("ws://localhost:9000/ocpp").with_connectors(2);
config.validate()?;
let (station, join) = Station::spawn(config, Arc::new(SystemClock));
station.send(Command::Connect).await?; // the station never connects by itself
println!("{:?}", station.snapshot().connectors.len());
station.shutdown().await?;
join.await?;
# Ok(())
# }
```

## Stability

Pre-1.0: the API may change between minor versions.

## Docs

- [Architecture](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md) (crate map, layer contracts, invariants)
- [Emulator guide](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/emulator.md), [scenarios](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/scenarios.md), [conformance notes](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/OCPP_1.6J_CONFORMANCE.md)
- API reference: <https://docs.rs/kirby-ocpp-core>

## Licence

MIT. The bundled OCPP JSON schemas belong to the Open Charge Alliance and are used under CC BY-ND 4.0, see `schemas/README.md`.
