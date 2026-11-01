//! Drives the fleet UI model against real stations and one mock CSMS: events and
//! commands stay with their own station.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kirby_ocpp_core::fleet::{Fleet, FleetSpec};
use kirby_ocpp_core::station::StationHandle;
use kirby_ocpp_core::ws::ConnectionState;
use kirby_ocpp_core::{Clock, StationConfig, SystemClock};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms};
use kirby_ocpp_tui::app::App;
use kirby_ocpp_tui::app::{Effect, Msg};
use kirby_ocpp_tui::fleet::{FleetApp, FleetEffect, FleetMsg, update_fleet};
use tokio::time::timeout;

const STEP: Duration = Duration::from_secs(10);

fn press(c: char) -> FleetMsg {
    FleetMsg::Key(KeyEvent::new_with_kind(
        KeyCode::Char(c),
        KeyModifiers::NONE,
        KeyEventKind::Press,
    ))
}

async fn wait_event(handle: &StationHandle) {
    let _ = handle.watch_snapshot().changed().await;
}

#[tokio::test]
async fn commands_and_events_follow_the_selected_station() {
    let csms = MockCsms::start(CsmsOptions::default()).await.unwrap();
    let base = StationConfig::new(csms.url()).without_persistence();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let mut fleet = Fleet::spawn(&base, &FleetSpec::new("UI-{n}", 2), &clock).unwrap();
    let handles: Vec<_> = fleet.stations().iter().map(|(_, h)| h.clone()).collect();
    let mut events: Vec<_> = handles.iter().map(StationHandle::subscribe).collect();
    let mut model = FleetApp::new(
        handles
            .iter()
            .map(|h| App::new(h.snapshot(), "ws://mock", "Acme"))
            .collect(),
    );

    // Select the second station and boot it from the UI ("1b": Station tab, Boot).
    update_fleet(&mut model, press('>'));
    assert_eq!(model.selected, 1);
    let mut effects = update_fleet(&mut model, press('1'));
    effects.extend(update_fleet(&mut model, press('c')));
    for effect in effects {
        if let FleetEffect::Station {
            station,
            effect: Effect::Send { command, .. },
        } = effect
        {
            assert_eq!(station, 1);
            handles[station].send(command).await.unwrap();
        }
    }
    timeout(STEP, async {
        loop {
            // Feed every station's events and snapshot into its own app.
            for (i, rx) in events.iter_mut().enumerate() {
                while let Ok(e) = rx.try_recv() {
                    update_fleet(
                        &mut model,
                        FleetMsg::Station {
                            station: i,
                            msg: Msg::Event(e),
                        },
                    );
                }
                update_fleet(
                    &mut model,
                    FleetMsg::Station {
                        station: i,
                        msg: Msg::Snapshot(Box::new(handles[i].snapshot())),
                    },
                );
            }
            if model.apps[1].snapshot.connection == ConnectionState::Connected {
                return;
            }
            // Wake on the next event of the station we are waiting for.
            let _ = timeout(Duration::from_millis(200), wait_event(&handles[1])).await;
        }
    })
    .await
    .expect("station 2 connects");

    assert_eq!(
        model.apps[0].snapshot.connection,
        ConnectionState::Disconnected
    );
    let mentions = |app: &App, id: &str| {
        app.log
            .all()
            .filter(|e| format!("{:?}", e.event).contains(id))
            .count()
    };
    assert_eq!(
        mentions(&model.apps[0], "UI-2"),
        0,
        "no leak into station 1"
    );
    assert_eq!(
        mentions(&model.apps[1], "UI-1"),
        0,
        "no leak into station 2"
    );
    assert!(
        model.apps[1].log.len() > model.apps[0].log.len(),
        "only station 2 connected and logged it"
    );
    assert!(model.apps[1].snapshot.charge_point_id.ends_with('2'));
    fleet.shutdown().await;
}
