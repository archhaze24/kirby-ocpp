//! Key handling of the OCPP 2.x model, per mode. Slice S1 of M6 fills it.

use crossterm::event::KeyEvent;

use super::app::{App2, Effect2};

/// Applies one key press to `app` and appends the effects it causes to `fx`.
pub(super) fn handle_key(app: &mut App2, key: KeyEvent, fx: &mut Vec<Effect2>) {
    let _ = (app, key, fx);
}
