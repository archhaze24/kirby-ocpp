//! The pure update function of the OCPP 2.x model. Slice S1 of M6 fills it.

use super::app::{App2, Effect2, Msg2};
use super::keys;

/// Applies one message to the model and returns what the event loop must do.
pub fn update2(app: &mut App2, msg: Msg2) -> Vec<Effect2> {
    let mut fx = Vec::new();
    match msg {
        Msg2::Key(key) => keys::handle_key(app, key, &mut fx),
        Msg2::Resize(w, h) => {
            app.size = (w, h);
            app.dirty = true;
        }
        Msg2::Snapshot(snapshot) => {
            app.snapshot = *snapshot;
            app.dirty = true;
        }
        Msg2::Signal => app.quit = true,
        Msg2::Paste(_)
        | Msg2::Tick
        | Msg2::Event(_)
        | Msg2::Scenario(_)
        | Msg2::ScenarioDone(_)
        | Msg2::ScenarioError(_)
        | Msg2::CommandDone { .. }
        | Msg2::Notice(..) => {}
    }
    fx
}
