//! Tests of the OCPP 2.x UI: `app.rs` (model) and `ui.rs` (render).

mod app;
mod ui;

use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::station2::Snapshot2;

use super::{App2, Effect2, Msg2, Tab2, update2};

#[test]
fn skeleton_updates_without_effects() {
    let config = StationConfig::new("ws://localhost/ocpp");
    let mut app = App2::new(Snapshot2::initial(&config), "ws://localhost/ocpp", "vendor");
    assert_eq!(app.tab, Tab2::Station);
    let none: Vec<Effect2> = Vec::new();
    assert_eq!(update2(&mut app, Msg2::Tick), none);
    assert_eq!(update2(&mut app, Msg2::Resize(80, 24)), none);
    assert_eq!(app.size, (80, 24));
    assert_eq!(update2(&mut app, Msg2::Signal), none);
    assert!(app.quit);
}
