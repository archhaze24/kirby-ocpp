# kirby-ocpp-tui

Terminal UI for the [`kirby-ocpp`](https://crates.io/crates/kirby-ocpp) charge point emulator, built on ratatui and crossterm.

**Most users want the binary**, not this library:

```
cargo install kirby-ocpp    # or: npm install -g kirby-ocpp
```

## What it is

- The emulator UI. The model is a pure `update(&mut App, Msg) -> Vec<Effect>`; the run loop executes the effects. The terminal is restored on every exit path.
- The UI reaches the station only through `kirby_ocpp_core::StationHandle`; it never touches internals.
- Generic pieces reused by `kirby-csms-tui`: terminal guard, `logview`, `text`, `clipboard`, `palette`.

It is not a station implementation (that is `kirby-ocpp-core`) and not a CLI (that is the `kirby-ocpp` binary).

## Entry points

`run(handle, TuiOptions)` runs the UI for one station, `run_fleet(&[handle], TuiOptions)` for several. Neither shuts the station down; the caller does.

```rust,no_run
use std::sync::Arc;
use kirby_ocpp_core::station::{Command, Station};
use kirby_ocpp_core::{StationConfig, SystemClock};
use kirby_ocpp_tui::TuiOptions;

# async fn go() -> Result<(), Box<dyn std::error::Error>> {
let (station, join) = Station::spawn(StationConfig::new("ws://localhost:9000/ocpp"), Arc::new(SystemClock));
station.send(Command::Connect).await?;
kirby_ocpp_tui::run(station.clone(), TuiOptions::default()).await?;
station.shutdown().await?;
join.await?;
# Ok(())
# }
```

## Stability

Pre-1.0 and mainly an internal part of the tool: the API may change between minor versions.

## Docs

- [Architecture](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md)
- [Emulator guide](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/emulator.md) (keys, actions)
- API reference: <https://docs.rs/kirby-ocpp-tui>

## License

MIT.
