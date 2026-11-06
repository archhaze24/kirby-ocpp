//! The model of a fleet UI: one [`App`] per station and a selector.
//!
//! Every station keeps its own [`App`] (snapshot, log buffer, mode, forms, scenario
//! progress), so nothing leaks between stations. [`update_fleet`] routes a message to
//! the right one and tags the resulting effects with the station they belong to. With
//! a single station the fleet model behaves exactly like a bare [`App`]: no selector
//! keys, no selector row.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kirby_ocpp_core::station::Snapshot;
use kirby_ocpp_core::ws::ConnectionState;

use crate::app::{App, Effect, Mode, Msg, NoticeLevel, update};
use crate::logview::{LOG_BYTE_BUDGET, LogStore};

/// Most log rows kept per station when there is more than one.
pub const FLEET_LOG_CAPACITY: usize = 500;
/// Fewest log rows kept per station, however large the fleet.
const FLEET_LOG_MIN_CAPACITY: usize = 50;
/// Log rows kept across the whole fleet (memory stays bounded as the fleet grows).
const FLEET_LOG_TOTAL_BUDGET: usize = 20_000;

/// Log rows kept per station in a fleet of `count` stations.
pub fn fleet_log_capacity(count: usize) -> usize {
    (FLEET_LOG_TOTAL_BUDGET / count.max(1)).clamp(FLEET_LOG_MIN_CAPACITY, FLEET_LOG_CAPACITY)
}

/// Approximate log bytes kept across the whole fleet.
const FLEET_LOG_TOTAL_BYTES: usize = 256 * 1024 * 1024;

/// Approximate log bytes kept per station in a fleet of `count` stations.
pub fn fleet_log_bytes(count: usize) -> usize {
    (FLEET_LOG_TOTAL_BYTES / count.max(1)).clamp(256 * 1024, LOG_BYTE_BUDGET)
}

/// What the event loop must do for the fleet model.
#[derive(Debug, Clone, PartialEq)]
pub enum FleetEffect {
    /// A per-station effect; `station` indexes [`FleetApp::apps`].
    Station {
        station: usize,
        effect: Effect,
    },
    ConnectAll,
    DisconnectAll,
}

/// Everything that can happen to the fleet model.
#[derive(Debug)]
pub enum FleetMsg {
    /// Keyboard input for the selected station (or the selector).
    Key(KeyEvent),
    Paste(String),
    Resize(u16, u16),
    Tick,
    /// A message for one station.
    Station {
        station: usize,
        msg: Msg,
    },
    /// SIGINT/SIGTERM.
    Signal,
}

#[derive(Debug)]
pub struct FleetApp {
    pub apps: Vec<App>,
    pub selected: usize,
    pub dirty: bool,
    pub quit: bool,
}

impl FleetApp {
    /// Builds the model from per-station apps. With more than one station each app gets
    /// the fleet help and a smaller log buffer.
    pub fn new(mut apps: Vec<App>) -> Self {
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
    pub fn current(&self) -> &App {
        &self.apps[self.selected]
    }

    #[allow(clippy::indexing_slicing, reason = "selected is kept below apps.len()")]
    pub fn current_mut(&mut self) -> &mut App {
        &mut self.apps[self.selected]
    }

    pub fn select(&mut self, index: usize) {
        if index < self.apps.len() && index != self.selected {
            self.selected = index;
            self.dirty = true;
        }
    }

    fn select_relative(&mut self, delta: isize) {
        let n = self.apps.len();
        self.select((self.selected + n).wrapping_add_signed(delta) % n);
    }
}

/// The selector keys, active in normal mode only: `<` `>` previous/next station,
/// Alt-1..9 jump to station 1..9, Alt-c / Alt-d connect / disconnect all stations.
fn selector_key(key: KeyEvent) -> Option<SelectorKey> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('<') if !ctrl && !alt => Some(SelectorKey::Step(-1)),
        KeyCode::Char('>') if !ctrl && !alt => Some(SelectorKey::Step(1)),
        KeyCode::Char(d @ '1'..='9') if alt && !ctrl => {
            Some(SelectorKey::Jump(usize::from(d as u8 - b'1')))
        }
        KeyCode::Char('c') if alt && !ctrl => Some(SelectorKey::ConnectAll),
        KeyCode::Char('d') if alt && !ctrl => Some(SelectorKey::DisconnectAll),
        _ => None,
    }
}

enum SelectorKey {
    Step(isize),
    Jump(usize),
    ConnectAll,
    DisconnectAll,
}

/// Folds one message into the fleet model.
pub fn update_fleet(fleet: &mut FleetApp, msg: FleetMsg) -> Vec<FleetEffect> {
    let mut out = Vec::new();
    match msg {
        FleetMsg::Key(key) => {
            let normal = matches!(fleet.current().mode, Mode::Normal);
            match fleet.is_fleet().then(|| selector_key(key)).flatten() {
                Some(sel) if normal => match sel {
                    SelectorKey::Step(d) => fleet.select_relative(d),
                    SelectorKey::Jump(i) => fleet.select(i),
                    SelectorKey::ConnectAll => {
                        out.push(FleetEffect::ConnectAll);
                        fleet
                            .current_mut()
                            .notify(NoticeLevel::Info, "connecting all stations...");
                    }
                    SelectorKey::DisconnectAll => {
                        out.push(FleetEffect::DisconnectAll);
                        fleet
                            .current_mut()
                            .notify(NoticeLevel::Info, "disconnecting all stations...");
                    }
                },
                _ => to_selected(fleet, Msg::Key(key), &mut out),
            }
        }
        FleetMsg::Paste(text) => to_selected(fleet, Msg::Paste(text), &mut out),
        FleetMsg::Resize(w, h) => {
            for app in &mut fleet.apps {
                update(app, Msg::Resize(w, h));
            }
        }
        FleetMsg::Tick => {
            for app in &mut fleet.apps {
                update(app, Msg::Tick);
            }
        }
        FleetMsg::Signal => {
            fleet.quit = true;
            fleet.current_mut().quit = true;
        }
        FleetMsg::Station { station, msg } => {
            let is_current = station == fleet.selected;
            // A snapshot of another station can change the selector row.
            let strip_changes = !is_current && matches!(msg, Msg::Snapshot(_));
            if let Some(app) = fleet.apps.get_mut(station) {
                let effects = update(app, msg);
                out.extend(
                    effects
                        .into_iter()
                        .map(|effect| FleetEffect::Station { station, effect }),
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

fn to_selected(fleet: &mut FleetApp, msg: Msg, out: &mut Vec<FleetEffect>) {
    let station = fleet.selected;
    let effects = update(fleet.current_mut(), msg);
    out.extend(
        effects
            .into_iter()
            .map(|effect| FleetEffect::Station { station, effect }),
    );
}

/// How the selector shows one station: connection glyph and whether a transaction runs.
pub fn station_marks(snapshot: &Snapshot) -> (char, bool) {
    let glyph = match snapshot.connection {
        ConnectionState::Connected => '+',
        ConnectionState::Connecting | ConnectionState::Backoff { .. } => '~',
        ConnectionState::Disconnected => '-',
    };
    let tx = snapshot.connectors.iter().any(|c| c.tx_id().is_some());
    (glyph, tx)
}

/// The slice `start..end` of items (with the given widths, one separator column
/// between them) that fits `avail` columns and contains `selected`.
pub fn visible_range(widths: &[usize], selected: usize, avail: usize) -> (usize, usize) {
    let n = widths.len();
    let selected = selected.min(n.saturating_sub(1));
    let w = |i: usize| widths.get(i).copied().unwrap_or(0) + 1;
    let (mut start, mut end) = (selected, selected + 1);
    let mut used = w(selected);
    loop {
        let mut grew = false;
        if start > 0 && used + w(start - 1) <= avail {
            start -= 1;
            used += w(start);
            grew = true;
        }
        if end < n && used + w(end) <= avail {
            used += w(end);
            end += 1;
            grew = true;
        }
        if !grew {
            return (start, end);
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use kirby_ocpp_core::StationConfig;
    use kirby_ocpp_core::station::{Command, Event, LogLevel, LogRecord};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn station_app(id: &str) -> App {
        let config = StationConfig::new("ws://csms.example/ocpp")
            .with_connectors(2)
            .with_charge_point_id(id);
        App::new(Snapshot::initial(&config), "ws://csms.example/ocpp", "Acme")
    }

    fn fleet_of(n: usize) -> FleetApp {
        FleetApp::new(
            (1..=n)
                .map(|i| station_app(&format!("CP-{i:03}")))
                .collect(),
        )
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> FleetMsg {
        FleetMsg::Key(KeyEvent::new_with_kind(
            code,
            modifiers,
            KeyEventKind::Press,
        ))
    }

    fn chr(c: char) -> FleetMsg {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn alt(c: char) -> FleetMsg {
        key(KeyCode::Char(c), KeyModifiers::ALT)
    }

    fn log_line(text: &str) -> Event {
        Event::Log(LogRecord {
            ts: Utc.timestamp_millis_opt(1_700_000_000_000).unwrap(),
            level: LogLevel::Info,
            text: text.into(),
        })
    }

    #[test]
    fn selector_keys_switch_stations_and_wrap() {
        let mut fleet = fleet_of(3);
        assert_eq!(fleet.selected, 0);
        update_fleet(&mut fleet, chr('>'));
        update_fleet(&mut fleet, chr('>'));
        assert_eq!(fleet.selected, 2);
        update_fleet(&mut fleet, chr('>'));
        assert_eq!(fleet.selected, 0);
        update_fleet(&mut fleet, chr('<'));
        assert_eq!(fleet.selected, 2);
        update_fleet(&mut fleet, alt('2'));
        assert_eq!(fleet.selected, 1);
        update_fleet(&mut fleet, alt('9'));
        assert_eq!(fleet.selected, 1, "out of range jump is ignored");
    }

    #[test]
    fn selector_keys_do_nothing_with_one_station() {
        let mut fleet = fleet_of(1);
        assert!(!fleet.is_fleet());
        assert!(!fleet.current().in_fleet);
        for msg in [chr('>'), chr('<'), alt('1'), alt('c'), alt('d')] {
            let effects = update_fleet(&mut fleet, msg);
            assert_eq!(effects.len(), 0);
        }
        assert_eq!(fleet.selected, 0);
        assert!(fleet.current().notice.is_none());
    }

    #[test]
    fn selector_keys_are_text_while_a_dialog_is_open() {
        let mut fleet = fleet_of(2);
        update_fleet(&mut fleet, chr(':'));
        assert!(matches!(fleet.current().mode, Mode::Palette(_)));
        update_fleet(&mut fleet, chr('>'));
        assert_eq!(fleet.selected, 0, "the key went to the palette");
        update_fleet(&mut fleet, key(KeyCode::Esc, KeyModifiers::NONE));
        update_fleet(&mut fleet, chr('>'));
        assert_eq!(fleet.selected, 1);
    }

    #[test]
    fn events_and_snapshots_stay_with_their_station() {
        let mut fleet = fleet_of(3);
        for (station, text) in [(0, "zero"), (2, "two"), (2, "two again")] {
            update_fleet(
                &mut fleet,
                FleetMsg::Station {
                    station,
                    msg: Msg::Event(log_line(text)),
                },
            );
        }
        let mut snap = fleet.apps[1].snapshot.clone();
        snap.connection = ConnectionState::Connected;
        update_fleet(
            &mut fleet,
            FleetMsg::Station {
                station: 1,
                msg: Msg::Snapshot(Box::new(snap)),
            },
        );
        let counts: Vec<usize> = fleet.apps.iter().map(|a| a.log.len()).collect();
        assert_eq!(counts, [1, 0, 2]);
        assert_eq!(
            fleet.apps[1].snapshot.connection,
            ConnectionState::Connected
        );
        assert_ne!(
            fleet.apps[0].snapshot.connection,
            ConnectionState::Connected
        );
        assert_ne!(
            fleet.apps[2].snapshot.connection,
            ConnectionState::Connected
        );
    }

    #[test]
    fn a_background_snapshot_redraws_the_selector_but_a_background_event_does_not() {
        let mut fleet = fleet_of(2);
        fleet.dirty = false;
        fleet.apps[0].dirty = false;
        fleet.apps[1].dirty = false;
        update_fleet(
            &mut fleet,
            FleetMsg::Station {
                station: 1,
                msg: Msg::Event(log_line("x")),
            },
        );
        assert!(!fleet.dirty);
        let snap = fleet.apps[1].snapshot.clone();
        update_fleet(
            &mut fleet,
            FleetMsg::Station {
                station: 1,
                msg: Msg::Snapshot(Box::new(snap)),
            },
        );
        assert!(fleet.dirty);
    }

    #[test]
    fn commands_go_to_the_selected_station() {
        let mut fleet = fleet_of(3);
        update_fleet(&mut fleet, chr('>'));
        // `a` is the "Add connector" accelerator of the Station tab.
        let effects = update_fleet(&mut fleet, chr('a'));
        let sent: Vec<_> = effects
            .iter()
            .filter_map(|e| match e {
                FleetEffect::Station {
                    station,
                    effect: Effect::Send { command, .. },
                } => Some((*station, command.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [(1, Command::AddConnector)], "{effects:?}");
        update_fleet(&mut fleet, chr('<'));
        let effects = update_fleet(&mut fleet, chr('a'));
        assert!(matches!(
            effects.as_slice(),
            [FleetEffect::Station { station: 0, .. }]
        ));
    }

    #[test]
    fn per_station_mode_and_filter_do_not_leak() {
        let mut fleet = fleet_of(2);
        update_fleet(&mut fleet, chr('?'));
        assert!(matches!(fleet.apps[1].mode, Mode::Normal));
        assert!(matches!(fleet.current().mode, Mode::Help));
        // Help is a dialog: the selector key does not switch away.
        update_fleet(&mut fleet, chr('>'));
        assert_eq!(fleet.selected, 0);
        update_fleet(&mut fleet, chr('?'));
        update_fleet(&mut fleet, chr('>'));
        assert!(matches!(fleet.apps[1].mode, Mode::Normal));
        assert!(matches!(fleet.apps[0].mode, Mode::Normal));
        // Open a form on station 0 is not visible on station 1.
        update_fleet(&mut fleet, chr('<'));
        update_fleet(&mut fleet, chr(':'));
        assert!(matches!(fleet.apps[0].mode, Mode::Palette(_)));
        assert!(matches!(fleet.apps[1].mode, Mode::Normal));
    }

    #[test]
    fn fleet_wide_actions_and_quit() {
        let mut fleet = fleet_of(3);
        assert_eq!(
            update_fleet(&mut fleet, alt('c')),
            [FleetEffect::ConnectAll]
        );
        assert_eq!(
            update_fleet(&mut fleet, alt('d')),
            [FleetEffect::DisconnectAll]
        );
        update_fleet(&mut fleet, chr('>'));
        assert!(!fleet.quit);
        update_fleet(&mut fleet, chr('q'));
        assert!(!fleet.quit, "a fleet asks before quitting");
        assert!(matches!(fleet.current().mode, Mode::Confirm(_)));
        update_fleet(&mut fleet, chr('y'));
        assert!(fleet.quit);

        let mut fleet = fleet_of(2);
        update_fleet(&mut fleet, FleetMsg::Signal);
        assert!(fleet.quit);
    }

    #[test]
    fn fleet_log_buffers_are_smaller() {
        let single = fleet_of(1);
        let fleet = fleet_of(2);
        assert_ne!(single.apps[0].log.capacity(), fleet.apps[0].log.capacity());
        assert_eq!(fleet.apps[0].log.capacity(), FLEET_LOG_CAPACITY);
    }

    #[test]
    fn fleet_log_capacity_scales_down_with_the_fleet() {
        assert_eq!(fleet_log_capacity(2), FLEET_LOG_CAPACITY);
        assert_eq!(fleet_log_capacity(40), 500);
        assert_eq!(fleet_log_capacity(100), 200);
        assert_eq!(fleet_log_capacity(1000), 50);
        assert_eq!(fleet_log_capacity(100_000), FLEET_LOG_MIN_CAPACITY);
        assert_eq!(fleet_log_capacity(0), FLEET_LOG_CAPACITY);
        let fleet = fleet_of(1000);
        assert_eq!(fleet.apps[0].log.capacity(), 50);
    }

    #[test]
    fn visible_range_keeps_the_selected_item_in_view() {
        let widths = [10; 10];
        assert_eq!(visible_range(&widths, 0, 35), (0, 3));
        let (s, e) = visible_range(&widths, 9, 35);
        assert_eq!((s, e), (7, 10));
        let (s, e) = visible_range(&widths, 5, 35);
        assert!(s <= 5 && 5 < e && e - s == 3);
        assert_eq!(
            visible_range(&widths, 4, 3),
            (4, 5),
            "always the selected one"
        );
        assert_eq!(visible_range(&[], 0, 10), (0, 1));
    }

    fn render(fleet: &FleetApp, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw_fleet(f, fleet)).unwrap();
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
        let text = render(&fleet, 100, 30);
        let first = text.lines().next().unwrap();
        assert!(first.contains("1 CP-001-"), "{first}");
        assert!(first.contains("2 CP-002+"), "{first}");
        assert!(first.contains("3 CP-003-"), "{first}");
        assert!(first.ends_with("1/3"), "{first}");
        // The body is the selected station's.
        assert!(text.contains("CP-001"));
    }

    #[test]
    fn selector_row_follows_the_selection_and_windows_long_fleets() {
        let mut fleet = fleet_of(40);
        fleet.select(30);
        let text = render(&fleet, 60, 20);
        let first = text.lines().next().unwrap();
        assert!(first.contains("31 CP-031"), "{first}");
        assert!(first.starts_with('<') && first.contains('>'), "{first}");
        assert!(!first.contains(" 1 CP-001"), "{first}");
        assert!(first.ends_with("31/40"), "{first}");
    }

    #[test]
    fn single_station_draws_without_selector_chrome() {
        let fleet = fleet_of(1);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| crate::ui::draw_fleet(f, &fleet)).unwrap();
        let fleet_buf = term.backend().buffer().clone();
        term.draw(|f| crate::ui::draw(f, fleet.current())).unwrap();
        assert_eq!(&fleet_buf, term.backend().buffer());
    }

    #[test]
    fn fleet_help_lists_the_selector_keys() {
        assert!(
            crate::ui::help_lines(true)
                .iter()
                .any(|l| l.contains("Alt-1"))
        );
        assert!(
            !crate::ui::help_lines(false)
                .iter()
                .any(|l| l.contains("Alt-1"))
        );
    }
}
