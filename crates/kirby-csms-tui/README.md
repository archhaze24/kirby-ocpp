# kirby-csms-tui

Terminal UI for the [`kirby-csms`](https://crates.io/crates/kirby-csms) central system, built on ratatui and crossterm. It shows the station list, station details, a merged or per-station frame log, command forms, response rules and security forms.

The model is a pure `update(&mut App, Msg) -> Vec<Effect>`. The UI reaches the engine only through `kirby_csms_core::CsmsHandle`, and commands run as background tasks so a slow station never blocks the screen.

This crate is an internal part of the tool and has no stable API. For the application itself install `kirby-csms`.

Project home and usage: <https://github.com/archhaze24/kirby-ocpp>. API documentation: <https://docs.rs/kirby-csms-tui>.

## Licence

MIT.
