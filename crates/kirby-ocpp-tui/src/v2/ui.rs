//! Drawing of the OCPP 2.x UI. `draw2` is a pure function of the [`App2`]; it never mutates
//! it. Slice S2 of M6 fills it.

use ratatui::Frame;

use super::app::App2;

/// Draws the whole UI: header, panes, footer and the overlay of the current mode.
pub fn draw2(f: &mut Frame<'_>, app: &App2) {
    let _ = (f, app);
}
