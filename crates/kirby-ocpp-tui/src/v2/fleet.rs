//! The fleet UI of OCPP 2.x stations: one [`App2`] per station, a selector row on top and the
//! selected station shown by the single-station UI of this module's siblings.
//!
//! It mirrors [`crate::fleet`] (same selector keys, same log sizing, same quit rules) and
//! reuses its protocol-neutral parts. Every station keeps its own model, so nothing leaks
//! between stations; [`update_fleet2`] routes a message to the right one and tags the effects
//! with the station they belong to.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crossterm::event::{Event as TermEvent, EventStream};
use futures_util::StreamExt;
use kirby_ocpp_core::scenario::CancelToken;
use kirby_ocpp_core::station2::{Snapshot2, Station2Handle};
use ratatui::Frame;
use tokio::sync::mpsc;

use super::app::{App2, Effect2, Mode2, Msg2};
use super::run::{Ctx, Out, execute, forward_station};
use super::ui::draw_in;
use super::update2;
use crate::app::NoticeLevel;
use crate::fleet::{
    SelectorKey, connection_glyph, fleet_log_bytes, fleet_log_capacity, selector_key,
    step_selection,
};
use crate::logview::LogStore;
use crate::terminal::TerminalGuard;
use crate::ui::{draw_selector, split_selector};
use crate::{EVENT_QUEUE, Inbound, TuiError, TuiOptions, text};

/// What the event loop must do for the fleet model.
#[derive(Debug, Clone, PartialEq)]
pub enum FleetEffect2 {
    /// A per-station effect; `station` indexes [`FleetApp2::apps`].
    Station {
        station: usize,
        effect: Effect2,
    },
    ConnectAll,
    DisconnectAll,
}

/// Everything that can happen to the fleet model.
#[derive(Debug)]
pub enum FleetMsg2 {
    /// Keyboard input for the selected station (or the selector).
    Key(crossterm::event::KeyEvent),
    Paste(String),
    Resize(u16, u16),
    Tick,
    /// A message for one station.
    Station {
        station: usize,
        msg: Msg2,
    },
    /// SIGINT/SIGTERM.
    Signal,
}

#[derive(Debug)]
pub struct FleetApp2 {
    pub apps: Vec<App2>,
    pub selected: usize,
    pub dirty: bool,
    pub quit: bool,
}

impl FleetApp2 {
    /// Builds the model from per-station apps. With more than one station each app gets the
    /// fleet help and a smaller log buffer.
    pub fn new(mut apps: Vec<App2>) -> Self {
        if apps.len() > 1 {
            let count = apps.len();
            let capacity = fleet_log_capacity(count);
            for app in &mut apps {
                app.in_fleet = true;
                app.log = LogStore::new(capacity).with_byte_budget(fleet_log_bytes(count));
            }
        }
        Self {
            apps,
            selected: 0,
            dirty: true,
            quit: false,
        }
    }

    /// More than one station: the selector is shown and its keys are active.
    pub fn is_fleet(&self) -> bool {
        self.apps.len() > 1
    }

    /// The app of the selected station.
    ///
    /// # Panics
    /// Never for a model built with at least one app (the loop guarantees it).
    #[allow(clippy::indexing_slicing, reason = "selected is kept below apps.len()")]
    pub fn current(&self) -> &App2 {
        &self.apps[self.selected]
    }

    #[allow(clippy::indexing_slicing, reason = "selected is kept below apps.len()")]
    pub fn current_mut(&mut self) -> &mut App2 {
        &mut self.apps[self.selected]
    }

    pub fn select(&mut self, index: usize) {
        if index < self.apps.len() && index != self.selected {
            self.selected = index;
            self.dirty = true;
        }
    }
}

/// How the selector shows one station: connection glyph and whether any EVSE has a
/// transaction.
pub fn station_marks2(snapshot: &Snapshot2) -> (char, bool) {
    let tx = snapshot.evses.iter().any(|e| e.transaction.is_some());
    (connection_glyph(&snapshot.connection), tx)
}

/// Folds one message into the fleet model.
pub fn update_fleet2(fleet: &mut FleetApp2, msg: FleetMsg2) -> Vec<FleetEffect2> {
    let mut out = Vec::new();
    match msg {
        FleetMsg2::Key(key) => {
            let normal = matches!(fleet.current().mode, Mode2::Normal);
            match fleet.is_fleet().then(|| selector_key(key)).flatten() {
                Some(sel) if normal => match sel {
                    SelectorKey::Step(d) => {
                        fleet.select(step_selection(fleet.selected, fleet.apps.len(), d));
                    }
                    SelectorKey::Jump(i) => fleet.select(i),
                    SelectorKey::ConnectAll => {
                        out.push(FleetEffect2::ConnectAll);
                        fleet
                            .current_mut()
                            .notify(NoticeLevel::Info, "connecting all stations...");
                    }
                    SelectorKey::DisconnectAll => {
                        out.push(FleetEffect2::DisconnectAll);
                        fleet
                            .current_mut()
                            .notify(NoticeLevel::Info, "disconnecting all stations...");
                    }
                },
                _ => to_selected(fleet, Msg2::Key(key), &mut out),
            }
        }
        FleetMsg2::Paste(text) => to_selected(fleet, Msg2::Paste(text), &mut out),
        FleetMsg2::Resize(w, h) => {
            for app in &mut fleet.apps {
                update2(app, Msg2::Resize(w, h));
            }
        }
        FleetMsg2::Tick => {
            for app in &mut fleet.apps {
                update2(app, Msg2::Tick);
            }
        }
        FleetMsg2::Signal => {
            fleet.quit = true;
            fleet.current_mut().quit = true;
        }
        FleetMsg2::Station { station, msg } => {
            let is_current = station == fleet.selected;
            // A snapshot of another station can change the selector row.
            let strip_changes = !is_current && matches!(msg, Msg2::Snapshot(_));
            if let Some(app) = fleet.apps.get_mut(station) {
                let effects = update2(app, msg);
                out.extend(
                    effects
                        .into_iter()
                        .map(|effect| FleetEffect2::Station { station, effect }),
                );
                if strip_changes {
                    fleet.dirty = true;
                }
            }
        }
    }
    if fleet.current().quit {
        fleet.quit = true;
    }
    if fleet.current().dirty {
        fleet.dirty = true;
    }
    out
}

fn to_selected(fleet: &mut FleetApp2, msg: Msg2, out: &mut Vec<FleetEffect2>) {
    let station = fleet.selected;
    let effects = update2(fleet.current_mut(), msg);
    out.extend(
        effects
            .into_iter()
            .map(|effect| FleetEffect2::Station { station, effect }),
    );
}

/// Draws the whole fleet UI: with several stations a selector row on top and the selected
/// station below; with one station exactly [`super::draw2`].
pub fn draw_fleet2(f: &mut Frame<'_>, fleet: &FleetApp2) {
    draw_fleet_colored(f, fleet);
    if fleet.current().mono {
        crate::theme::apply_monochrome(f.buffer_mut());
    }
}

fn draw_fleet_colored(f: &mut Frame<'_>, fleet: &FleetApp2) {
    if !fleet.is_fleet() || f.area().height < 2 {
        draw_in(f, fleet.current(), f.area());
        return;
    }
    let Some([strip, rest]) = split_selector(f) else {
        return;
    };
    let stations = fleet.apps.iter().map(|app| {
        let (glyph, tx) = station_marks2(&app.snapshot);
        (app.snapshot.charge_point_id.as_str(), glyph, tx)
    });
    draw_selector(f, stations, fleet.selected, strip);
    draw_in(f, fleet.current(), rest);
}

/// Runs the UI over one or more OCPP 2.x stations until the user quits. With more than one
/// station a selector row is shown. Does not shut the stations down; the caller does.
///
/// # Errors
///
/// Fails when the terminal cannot be set up or drawn to.
pub async fn run_fleet2(stations: &[Station2Handle], options: TuiOptions) -> Result<(), TuiError> {
    // Subscribe before anything slow (the terminal setup): the first station may already be
    // booting, and an event sent before the subscription is lost to the log and the counters.
    let (event_tx, mut event_rx) = mpsc::channel::<Inbound<Msg2>>(EVENT_QUEUE);
    for (station, handle) in stations.iter().enumerate() {
        forward_station(handle, station, &event_tx);
    }
    let mut guard = TerminalGuard::enter()?;
    let apps: Vec<App2> = stations
        .iter()
        .map(|handle| {
            let config = handle.config();
            App2::new(
                handle.snapshot(),
                text::redact_url(&config.url),
                config.vendor.clone(),
            )
        })
        .collect();
    let mut model = FleetApp2::new(apps);
    for app in &mut model.apps {
        app.mono = options.monochrome;
        app.time = options.time;
    }
    if let Ok(size) = guard.terminal().size() {
        for app in &mut model.apps {
            app.size = (size.width, size.height);
        }
    }

    // User-action results (few, never dropped) and forwarded station events (bounded).
    let (user_tx, mut user_rx) = mpsc::unbounded_channel::<Inbound<Msg2>>();
    let mut ctxs: Vec<Ctx> = Vec::with_capacity(stations.len());
    for (station, handle) in stations.iter().enumerate() {
        ctxs.push(Ctx {
            handle: handle.clone(),
            out: Out {
                tx: user_tx.clone(),
                station,
            },
            scenario_running: Arc::new(AtomicBool::new(false)),
            cancel: CancelToken::new(),
        });
    }
    let mut gone = 0;
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(options.tick_rate);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let signal = crate::shutdown_signal();
    tokio::pin!(signal);

    while !model.quit {
        let msg = tokio::select! {
            ev = keys.next() => match ev {
                Some(Ok(TermEvent::Key(k))) => FleetMsg2::Key(k),
                Some(Ok(TermEvent::Paste(s))) => FleetMsg2::Paste(s),
                Some(Ok(TermEvent::Resize(w, h))) => FleetMsg2::Resize(w, h),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            Some(inbound) = async {
                tokio::select! {
                    m = user_rx.recv() => m,
                    m = event_rx.recv() => m,
                }
            } => match inbound {
                Inbound::Msg(station, msg) => FleetMsg2::Station { station, msg },
                Inbound::Gone => {
                    gone += 1;
                    if gone >= ctxs.len() {
                        break;
                    }
                    continue;
                }
            },
            _ = tick.tick() => FleetMsg2::Tick,
            () = &mut signal => FleetMsg2::Signal,
        };
        let is_tick = matches!(msg, FleetMsg2::Tick);
        for effect in update_fleet2(&mut model, msg) {
            match effect {
                FleetEffect2::Station { station, effect } => {
                    if let Some(ctx) = ctxs.get_mut(station) {
                        execute(effect, ctx, &mut guard).await;
                    }
                }
                FleetEffect2::ConnectAll => {
                    for ctx in &ctxs {
                        let (handle, out) = (ctx.handle.clone(), ctx.out.clone());
                        tokio::spawn(async move {
                            if let Err(e) = handle.connect().await {
                                out.send(Msg2::Notice(
                                    NoticeLevel::Error,
                                    format!("connect failed: {e}"),
                                ));
                            }
                        });
                    }
                }
                FleetEffect2::DisconnectAll => {
                    for ctx in &ctxs {
                        let (handle, out) = (ctx.handle.clone(), ctx.out.clone());
                        tokio::spawn(async move {
                            if let Err(e) = handle.disconnect().await {
                                out.send(Msg2::Notice(
                                    NoticeLevel::Error,
                                    format!("disconnect failed: {e}"),
                                ));
                            }
                        });
                    }
                }
            }
        }
        if is_tick && model.dirty {
            guard.terminal().draw(|f| draw_fleet2(f, &model))?;
            model.dirty = false;
            model.current_mut().dirty = false;
        }
    }
    for ctx in &ctxs {
        ctx.cancel.cancel();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use kirby_ocpp_core::StationConfig;
    use kirby_ocpp_core::station2::EvseSnapshot;
    use kirby_ocpp_core::ws::ConnectionState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    use super::*;

    fn station_app(id: &str) -> App2 {
        let mut config = StationConfig::new("ws://csms.example/ocpp").with_charge_point_id(id);
        config.protocol = kirby_ocpp_core::ocpp::Protocol::V21;
        config.ws_subprotocol = "ocpp2.1".to_owned();
        let mut snapshot = Snapshot2::initial(&config);
        snapshot.evses = vec![evse(None)];
        App2::new(snapshot, "ws://csms.example/ocpp", "Acme")
    }

    /// An EVSE built from its wire shape, so new defaulted fields need no change here.
    fn evse(transaction: Option<&str>) -> EvseSnapshot {
        let transaction = transaction.map(|id| {
            json!({
                "transaction_id": id,
                "next_seq_no": 1,
                "charging_state": "Charging",
                "id_token": null,
                "started_at": "2026-01-01T00:00:00Z",
                "energy_wh": 0,
                "remote_start_id": null,
            })
        });
        serde_json::from_value(json!({
            "id": 1,
            "operative": true,
            "connectors": [{"id": 1, "status": "Available", "ev_connected": false, "fault": false}],
            "transaction": transaction,
            "meter_wh": 0,
        }))
        .unwrap()
    }

    fn fleet_of(n: usize) -> FleetApp2 {
        FleetApp2::new(
            (1..=n)
                .map(|i| station_app(&format!("CP-{i:03}")))
                .collect(),
        )
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> FleetMsg2 {
        FleetMsg2::Key(KeyEvent::new_with_kind(
            code,
            modifiers,
            KeyEventKind::Press,
        ))
    }

    fn chr(c: char) -> FleetMsg2 {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn alt(c: char) -> FleetMsg2 {
        key(KeyCode::Char(c), KeyModifiers::ALT)
    }

    fn snapshot_msg(station: usize, snapshot: Snapshot2) -> FleetMsg2 {
        FleetMsg2::Station {
            station,
            msg: Msg2::Snapshot(Box::new(snapshot)),
        }
    }

    #[test]
    fn marks_show_the_connection_and_any_evse_transaction() {
        let mut snapshot = station_app("CP-001").snapshot;
        assert_eq!(station_marks2(&snapshot), ('-', false));
        snapshot.connection = ConnectionState::Connecting;
        assert_eq!(station_marks2(&snapshot), ('~', false));
        snapshot.connection = ConnectionState::Backoff { attempt: 2 };
        assert_eq!(station_marks2(&snapshot), ('~', false));
        snapshot.connection = ConnectionState::Connected;
        assert_eq!(station_marks2(&snapshot), ('+', false));
        snapshot.evses = vec![evse(None), evse(Some("tx-1"))];
        assert_eq!(station_marks2(&snapshot), ('+', true));
    }

    #[test]
    fn selector_keys_switch_stations_and_wrap() {
        let mut fleet = fleet_of(3);
        assert!(fleet.apps.iter().all(|a| a.in_fleet));
        update_fleet2(&mut fleet, chr('>'));
        update_fleet2(&mut fleet, chr('>'));
        assert_eq!(fleet.selected, 2);
        update_fleet2(&mut fleet, chr('>'));
        assert_eq!(fleet.selected, 0);
        update_fleet2(&mut fleet, chr('<'));
        assert_eq!(fleet.selected, 2);
        update_fleet2(&mut fleet, alt('2'));
        assert_eq!(fleet.selected, 1);
        update_fleet2(&mut fleet, alt('9'));
        assert_eq!(fleet.selected, 1, "out of range jump is ignored");
    }

    #[test]
    fn selector_keys_do_nothing_with_one_station() {
        let mut fleet = fleet_of(1);
        assert!(!fleet.is_fleet());
        assert!(!fleet.current().in_fleet);
        for msg in [chr('>'), chr('<'), alt('1'), alt('c'), alt('d')] {
            assert_eq!(update_fleet2(&mut fleet, msg).len(), 0);
        }
        assert_eq!(fleet.selected, 0);
        assert!(fleet.current().notice.is_none());
    }

    #[test]
    fn selector_keys_are_text_while_a_dialog_is_open() {
        let mut fleet = fleet_of(2);
        update_fleet2(&mut fleet, chr(':'));
        assert!(matches!(fleet.current().mode, Mode2::Palette(_)));
        update_fleet2(&mut fleet, chr('>'));
        assert_eq!(fleet.selected, 0, "the key went to the palette");
        update_fleet2(&mut fleet, key(KeyCode::Esc, KeyModifiers::NONE));
        update_fleet2(&mut fleet, chr('>'));
        assert_eq!(fleet.selected, 1);
    }

    #[test]
    fn snapshots_stay_with_their_station_and_redraw_the_selector() {
        let mut fleet = fleet_of(3);
        fleet.dirty = false;
        for app in &mut fleet.apps {
            app.dirty = false;
        }
        let mut snapshot = fleet.apps[1].snapshot.clone();
        snapshot.connection = ConnectionState::Connected;
        update_fleet2(&mut fleet, snapshot_msg(1, snapshot));
        assert_eq!(
            fleet.apps[1].snapshot.connection,
            ConnectionState::Connected
        );
        for other in [0, 2] {
            assert_ne!(
                fleet.apps[other].snapshot.connection,
                ConnectionState::Connected
            );
        }
        assert!(fleet.dirty, "a background snapshot redraws the selector");
    }

    #[test]
    fn keys_drive_the_selected_station_only() {
        let mut fleet = fleet_of(2);
        update_fleet2(&mut fleet, chr('>'));
        update_fleet2(&mut fleet, chr('?'));
        assert!(matches!(fleet.apps[1].mode, Mode2::Help(_)));
        assert!(matches!(fleet.apps[0].mode, Mode2::Normal));
        // Help is a dialog: the selector key does not switch away.
        update_fleet2(&mut fleet, chr('<'));
        assert_eq!(fleet.selected, 1);
    }

    #[test]
    fn fleet_wide_actions_and_quit() {
        let mut fleet = fleet_of(3);
        assert_eq!(
            update_fleet2(&mut fleet, alt('c')),
            [FleetEffect2::ConnectAll]
        );
        assert_eq!(
            update_fleet2(&mut fleet, alt('d')),
            [FleetEffect2::DisconnectAll]
        );
        update_fleet2(&mut fleet, chr('>'));
        update_fleet2(&mut fleet, chr('q'));
        assert!(!fleet.quit, "a fleet asks before quitting");
        assert!(matches!(fleet.current().mode, Mode2::Confirm(_)));
        update_fleet2(&mut fleet, chr('y'));
        assert!(fleet.quit);

        let mut fleet = fleet_of(2);
        update_fleet2(&mut fleet, FleetMsg2::Signal);
        assert!(fleet.quit);

        let mut single = fleet_of(1);
        update_fleet2(&mut single, chr('q'));
        assert!(single.quit, "one station quits at once");
    }

    #[test]
    fn fleet_log_buffers_follow_the_fleet_size() {
        let single = fleet_of(1);
        let fleet = fleet_of(2);
        assert_ne!(single.apps[0].log.capacity(), fleet.apps[0].log.capacity());
        assert_eq!(fleet.apps[0].log.capacity(), fleet_log_capacity(2));
        assert_eq!(
            fleet_of(1000).apps[0].log.capacity(),
            fleet_log_capacity(1000)
        );
    }

    fn render(fleet: &FleetApp2, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw_fleet2(f, fleet)).unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn selector_row_lists_stations_with_state() {
        let mut fleet = fleet_of(3);
        fleet.apps[1].snapshot.connection = ConnectionState::Connected;
        fleet.apps[2].snapshot.evses = vec![evse(Some("tx-1"))];
        let text = render(&fleet, 100, 30);
        let first = text.lines().next().unwrap();
        assert!(first.contains("CP-001 ○"), "{first}");
        assert!(first.contains("CP-002 ●"), "{first}");
        assert!(first.contains("CP-003 ○*"), "{first}");
        assert!(
            !first.contains("1 CP-001"),
            "no digits in the strip: {first}"
        );
        assert!(first.contains(" 1/3"), "{first}");
        // The legend names the markers in use, and only those.
        assert!(
            first.contains("● connected") && first.contains("○ offline"),
            "{first}"
        );
        assert!(
            first.contains("* transaction") && !first.contains("◐"),
            "{first}"
        );
        assert!(text.contains("CP-001"));
    }

    #[test]
    fn selector_row_sanitizes_station_ids() {
        let mut fleet = fleet_of(2);
        fleet.apps[1].snapshot.charge_point_id = "CP\u{1b}[31m-2".to_owned();
        let text = render(&fleet, 100, 30);
        let first = text.lines().next().unwrap();
        assert!(!first.contains('\u{1b}'), "{first:?}");
    }

    #[test]
    fn selector_row_follows_the_selection_and_windows_long_fleets() {
        let mut fleet = fleet_of(40);
        fleet.select(30);
        let text = render(&fleet, 60, 20);
        let first = text.lines().next().unwrap();
        assert!(first.contains("CP-031"), "{first}");
        assert!(first.starts_with('‹') && first.contains('›'), "{first}");
        assert!(first.contains("31/40"), "{first}");
    }

    #[test]
    fn single_station_draws_without_selector_chrome() {
        let fleet = fleet_of(1);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw_fleet2(f, &fleet)).unwrap();
        let fleet_buf = term.backend().buffer().clone();
        term.draw(|f| super::super::draw2(f, fleet.current()))
            .unwrap();
        assert_eq!(&fleet_buf, term.backend().buffer());
    }

    #[test]
    fn fleet_help_lists_the_selector_keys() {
        use super::super::ui::help_sections2;
        let has = |fleet: bool| {
            let mut app = fleet_of(1).apps.remove(0);
            app.in_fleet = fleet;
            help_sections2(&app)
                .iter()
                .flat_map(|s| s.lines.iter())
                .any(|l| l.contains("Alt-1"))
        };
        assert!(has(true));
        assert!(!has(false));
    }
}
