//! Key handling. Each [`Mode`] owns its keys; nothing falls through to another mode.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirby_ocpp_tui::app::{Detail, NoticeLevel};
use kirby_ocpp_tui::text::TextInput;

use crate::app::{
    App, Effect, Focus, Input, InputTarget, LogView, Mode, filter_label, next_filter,
};

/// Rows a page key moves.
const PAGE: isize = 10;

pub fn handle(app: &mut App, key: KeyEvent, fx: &mut Vec<Effect>) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    match std::mem::replace(&mut app.mode, Mode::Normal) {
        Mode::Normal => normal(app, key, fx),
        Mode::Input(input) => text_input(app, input, key, fx),
        Mode::Help => {
            if !matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?' | 'q') | KeyCode::Enter
            ) {
                app.mode = Mode::Help;
            }
        }
        Mode::Detail(detail) => detail_mode(app, detail, key, fx),
    }
}

/// Pasted text goes into an open input; elsewhere it is ignored.
pub fn paste(app: &mut App, text: &str, fx: &mut Vec<Effect>) {
    if let Mode::Input(input) = &mut app.mode {
        input.text.insert_str(&text.replace(['\n', '\r'], " "));
        let (target, value) = (input.target, input.text.text().to_owned());
        apply_input(app, target, &value, fx);
    }
}

fn normal(app: &mut App, key: KeyEvent, fx: &mut Vec<Effect>) {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return;
    }
    match key.code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char('/') => open_input(app, input_target_for_focus(app.focus)),
        KeyCode::Char('s') if app.focus == Focus::Log => open_input(app, InputTarget::LogSearch),
        KeyCode::Tab | KeyCode::Right => app.focus = Focus::Log,
        KeyCode::BackTab | KeyCode::Left => app.focus = Focus::Stations,
        KeyCode::Up | KeyCode::Char('k') => step(app, -1, fx),
        KeyCode::Down | KeyCode::Char('j') => step(app, 1, fx),
        KeyCode::PageUp => step(app, -PAGE, fx),
        KeyCode::PageDown => step(app, PAGE, fx),
        KeyCode::Home => match app.focus {
            Focus::Stations => app.select_row(0, fx),
            Focus::Log => app.log_mut().select_first(),
        },
        KeyCode::End => match app.focus {
            Focus::Stations => app.select_row(app.visible_len().saturating_sub(1), fx),
            Focus::Log => app.log_mut().follow_tail(),
        },
        KeyCode::Char('G') => app.log_mut().follow_tail(),
        KeyCode::Char('a') => {
            app.view = match app.view {
                LogView::Station => LogView::All,
                LogView::All => LogView::Station,
            };
            let text = match app.view {
                LogView::Station => "log: selected station",
                LogView::All => "log: all stations",
            };
            app.notify(NoticeLevel::Info, text);
        }
        KeyCode::Char('f') => {
            let next = next_filter(app.log().filter);
            app.log_mut().set_filter(next);
            app.notify(NoticeLevel::Info, format!("filter: {}", filter_label(next)));
        }
        KeyCode::Char('p') => {
            app.log_mut().toggle_follow();
            let text = if app.log().follow {
                "following"
            } else {
                "paused"
            };
            app.notify(NoticeLevel::Info, text);
        }
        KeyCode::Char('y') => copy_row(app, fx),
        KeyCode::Char('Y') => copy_visible(app, fx),
        KeyCode::Esc => {
            if !app.log().search.is_empty() {
                app.log_mut().search.clear();
                app.notify(NoticeLevel::Info, "search cleared");
            } else if !app.station_filter.is_empty() {
                app.set_station_filter("", fx);
                app.notify(NoticeLevel::Info, "filter cleared");
            }
        }
        KeyCode::Enter if app.focus == Focus::Log => app.open_detail(),
        _ => {}
    }
}

fn input_target_for_focus(focus: Focus) -> InputTarget {
    match focus {
        Focus::Stations => InputTarget::Stations,
        Focus::Log => InputTarget::LogSearch,
    }
}

fn open_input(app: &mut App, target: InputTarget) {
    let current = match target {
        InputTarget::Stations => app.station_filter.clone(),
        InputTarget::LogSearch => app.log().search.clone(),
    };
    app.mode = Mode::Input(Input {
        target,
        text: TextInput::new(current),
    });
}

fn step(app: &mut App, delta: isize, fx: &mut Vec<Effect>) {
    match app.focus {
        Focus::Stations => app.move_cursor(delta, fx),
        Focus::Log => app.log_mut().move_selection(delta),
    }
}

fn copy_row(app: &App, fx: &mut Vec<Effect>) {
    if let Some(entry) = app.log().selected() {
        fx.push(Effect::Copy {
            text: app.entry_text(entry),
            what: "row".to_owned(),
        });
    }
}

fn copy_visible(app: &App, fx: &mut Vec<Effect>) {
    let text: String = app
        .log()
        .visible()
        .into_iter()
        .map(|e| app.entry_text(e) + "\n")
        .collect();
    if !text.is_empty() {
        fx.push(Effect::Copy {
            text,
            what: "visible rows".to_owned(),
        });
    }
}

/// Applies the text of an input live, so the list and log narrow as one types.
fn apply_input(app: &mut App, target: InputTarget, text: &str, fx: &mut Vec<Effect>) {
    match target {
        InputTarget::Stations => app.set_station_filter(text, fx),
        InputTarget::LogSearch => text.clone_into(&mut app.log_mut().search),
    }
}

fn text_input(app: &mut App, mut input: Input, key: KeyEvent, fx: &mut Vec<Effect>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            apply_input(app, input.target, "", fx);
            return;
        }
        KeyCode::Enter => {
            let n = match input.target {
                InputTarget::Stations => app.visible_len(),
                InputTarget::LogSearch => app.log().visible_count(),
            };
            if !input.text.is_empty() {
                app.notify(NoticeLevel::Info, format!("{n} matching rows"));
            }
            return;
        }
        KeyCode::Char('u') if ctrl => input.text = TextInput::new(""),
        KeyCode::Char(_) if ctrl || key.modifiers.contains(KeyModifiers::ALT) => {}
        KeyCode::Char(c) => input.text.insert_char(c),
        KeyCode::Backspace => input.text.backspace(),
        KeyCode::Delete => input.text.delete(),
        KeyCode::Left => input.text.left(),
        KeyCode::Right => input.text.right(),
        KeyCode::Home => input.text.home(),
        KeyCode::End => input.text.end(),
        _ => {}
    }
    let (target, value) = (input.target, input.text.text().to_owned());
    apply_input(app, target, &value, fx);
    app.mode = Mode::Input(input);
}

fn detail_mode(app: &mut App, mut d: Detail, key: KeyEvent, fx: &mut Vec<Effect>) {
    let max = d.lines.len().saturating_sub(1);
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => return,
        KeyCode::Up | KeyCode::Char('k') => d.scroll = d.scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => d.scroll = (d.scroll + 1).min(max),
        KeyCode::PageUp => d.scroll = d.scroll.saturating_sub(10),
        KeyCode::PageDown => d.scroll = (d.scroll + 10).min(max),
        KeyCode::Home | KeyCode::Char('g') => d.scroll = 0,
        KeyCode::End | KeyCode::Char('G') => d.scroll = max,
        KeyCode::Char('c' | 'y') => fx.push(Effect::Copy {
            text: d.copy.clone(),
            what: "detail".to_owned(),
        }),
        _ => {}
    }
    app.mode = Mode::Detail(d);
}

/// The key reference shown by `?`.
pub const HELP: &[&str] = &[
    "Up Down j k             move in the focused pane",
    "PgUp PgDn Home End      page / first / last (log: oldest / newest)",
    "Left Right Tab          change focused pane (stations, log)",
    "/                       filter stations by id (log pane: search the log)",
    "Esc                     clear the log search, then the station filter",
    "a                       log of the selected station <-> all stations",
    "?                       this help",
    "q or Ctrl-C             quit the UI (the server keeps its own lifecycle)",
    "",
    "Log pane: Enter detail (scroll, c copies), y copy row, Y copy visible rows,",
    "     f filter (all, calls, errors, from stations), s search, p pause/follow,",
    "     G newest",
    "",
    "List: dot green connected, dim disconnected, yellow replaced; tx = active transactions",
];
