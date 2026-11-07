# kirby-csms-tui

Terminal UI for the [`kirby-csms`](https://crates.io/crates/kirby-csms) central system, built on ratatui and crossterm: station list, station details, merged or per-station frame log, command forms, response rules and security forms.

**Most users want the binary**, not this library:

```
cargo install kirby-csms    # or: npm install -g kirby-csms
```

## What it is

- The CSMS UI. The model is a pure `update(&mut App, Msg) -> Vec<Effect>`; commands run as background tasks, so a slow station never blocks the screen.
- It reaches the engine only through `kirby_csms_core::CsmsHandle`.
- It reuses terminal pieces from `kirby-ocpp-tui` (terminal guard, log view, clipboard).

It is not the server (that is `kirby-csms-core`) and not a CLI (that is the `kirby-csms` binary).

## Entry points

`run(CsmsHandle, TuiOptions)` enters the terminal and runs until the user quits. It does not shut the server down; the caller does.

```rust,no_run
use kirby_csms_core::{CsmsConfig, CsmsServer};
use kirby_csms_tui::TuiOptions;

# async fn go() -> Result<(), Box<dyn std::error::Error>> {
let csms = CsmsServer::start(CsmsConfig::new("0.0.0.0:9000".parse()?)).await?;
kirby_csms_tui::run(csms.clone(), TuiOptions::default()).await?;
csms.shutdown().await;
# Ok(())
# }
```

## Stability

Pre-1.0 and mainly an internal part of the tool: the API may change between minor versions.

## Docs

- [Architecture](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/architecture.md)
- [CSMS guide](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/csms.md) (keys, forms), [HTTP control API](https://github.com/archhaze24/kirby-ocpp/blob/main/docs/api.md)
- API reference: <https://docs.rs/kirby-csms-tui>

## License

MIT.
