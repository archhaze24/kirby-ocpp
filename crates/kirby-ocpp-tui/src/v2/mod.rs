//! Terminal UI for OCPP 2.x stations, next to the 1.6 UI of [`crate::app`].
//!
//! It has its own model ([`App2`], [`Msg2`], [`Effect2`], the pure [`update2`]) and its own
//! loop ([`run2`]) and shares the widgets of the 1.6 UI: the form engine
//! ([`crate::forms`]), the overlay drawing functions ([`crate::ui::overlays`]), the palette
//! ([`crate::palette`]), the log view ([`crate::logview`]) and the terminal guard.
//!
//! File ownership: `app.rs`, `actions.rs`, `keys.rs`, `forms.rs`, `update.rs` and
//! `tests/app.rs` are the model; `ui.rs`, `panes.rs`, `browser.rs`, `run.rs` and
//! `tests/ui.rs` are the view and the loop.

mod actions;
mod app;
mod browser;
mod fleet;
mod forms;
mod keys;
mod panes;
mod run;
#[cfg(test)]
mod tests;
mod ui;
mod update;

pub use actions::{ActionDef2, ActionId2, Tab2, catalog2};
pub use app::{App2, Confirm2, ConfirmAction2, Effect2, Mode2, Msg2};
pub use browser::{BrowserEvent, DeviceBrowser};
pub use fleet::{
    FleetApp2, FleetEffect2, FleetMsg2, draw_fleet2, run_fleet2, station_marks2, update_fleet2,
};
pub use run::run2;
pub use ui::draw2;
pub use update::update2;
