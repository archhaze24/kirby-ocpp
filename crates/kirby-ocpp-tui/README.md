# kirby-ocpp-tui

Terminal UI for the [`kirby-ocpp`](https://crates.io/crates/kirby-ocpp) charge point emulator, built on ratatui and crossterm.

The model is a pure `update(&mut App, Msg) -> Vec<Effect>`; the run loop executes the effects. The UI reaches the station only through `kirby_ocpp_core::StationHandle`. The terminal is restored on every exit path.

It also holds generic building blocks that `kirby-csms-tui` reuses: a terminal guard, a log view, text helpers, clipboard access and a command palette.

This crate is an internal part of the tool and has no stable API. For the application itself install `kirby-ocpp`.

Project home and usage: <https://github.com/archhaze24/kirby-ocpp>. API documentation: <https://docs.rs/kirby-ocpp-tui>.

## Licence

MIT.
