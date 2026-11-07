//! Drawing. `draw` is a pure function of the [`App`]; it never mutates it.

mod browser;
mod overlays;
mod panes;

pub use overlays::help_lines;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::actions::Tab;
use crate::app::{App, ConfirmAction, Mode, NoticeLevel};
use crate::fleet::{FleetApp, station_marks, visible_range};
use crate::text::{sanitize, truncate, width};

/// Width from which the three-column layout is used.
pub const WIDE: u16 = 110;
pub const MIN_SIZE: (u16, u16) = (24, 8);

pub fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

pub fn draw(f: &mut Frame<'_>, app: &App) {
    draw_in(f, app, f.area());
}

/// Draws the whole fleet UI: with several stations a selector row on top and the
/// selected station below; with one station exactly [`draw`].
pub fn draw_fleet(f: &mut Frame<'_>, fleet: &FleetApp) {
    if !fleet.is_fleet() || f.area().height < 2 {
        draw_in(f, fleet.current(), f.area());
        return;
    }
    // The selector row takes one line: a fleet needs one more row than a single station.
    let need = (MIN_SIZE.0, MIN_SIZE.1 + 1);
    if f.area().width < need.0 || f.area().height < need.1 {
        too_small(f, f.area(), need);
        return;
    }
    let [strip, rest] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(f.area());
    draw_selector(f, fleet, strip);
    draw_in(f, fleet.current(), rest);
}

/// `n:id<glyph>[*]` per station, windowed around the selected one, count on the right.
fn draw_selector(f: &mut Frame<'_>, fleet: &FleetApp, area: Rect) {
    let total = fleet.apps.len();
    let count = format!(" {}/{total}", fleet.selected + 1);
    let avail = usize::from(area.width).saturating_sub(width(&count) + 4);
    let labels: Vec<(String, Color)> = fleet
        .apps
        .iter()
        .enumerate()
        .map(|(i, app)| {
            let (glyph, tx) = station_marks(&app.snapshot);
            let color = match glyph {
                '+' => Color::Green,
                '~' => Color::Yellow,
                _ => Color::Red,
            };
            let tx = if tx { "*" } else { "" };
            let id = crate::text::sanitize(&app.snapshot.charge_point_id);
            (format!(" {} {id}{glyph}{tx} ", i + 1), color)
        })
        .collect();
    let widths: Vec<usize> = labels.iter().map(|(l, _)| width(l)).collect();
    let (start, end) = visible_range(&widths, fleet.selected, avail);
    let mut spans = vec![Span::styled(if start > 0 { "< " } else { "  " }, dim())];
    for (i, (label, color)) in labels.iter().enumerate().take(end).skip(start) {
        let style = if i == fleet.selected {
            Style::new().fg(Color::Black).bg(*color)
        } else {
            Style::new().fg(*color)
        };
        spans.push(Span::styled(truncate(label, avail.max(1)), style));
        spans.push(Span::raw(" "));
    }
    if end < total {
        spans.push(Span::styled("> ", dim()));
    }
    spans.push(Span::styled(count, dim()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Popup width: `want` cells, or the whole width when the terminal is barely wider, so the
/// popup never leaves a one-cell strip of the pane borders behind it.
pub fn popup_width(area_width: u16, want: u16) -> u16 {
    if area_width < want.saturating_add(4) {
        area_width
    } else {
        want
    }
}

/// The notice drawn instead of the UI in a terminal below the minimum size. The needed size
/// comes first, so it survives a very narrow terminal.
pub fn too_small(f: &mut Frame<'_>, area: Rect, need: (u16, u16)) {
    let msg = format!(
        "need {}x{}, have {}x{}",
        need.0, need.1, area.width, area.height
    );
    f.render_widget(
        Paragraph::new(truncate(&msg, usize::from(area.width))),
        area,
    );
}

fn draw_in(f: &mut Frame<'_>, app: &App, area: Rect) {
    if area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1 {
        too_small(f, area, MIN_SIZE);
        return;
    }
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(area);
    draw_header(f, app, header);
    draw_body(f, app, body);
    draw_footer(f, app, footer);
    overlays::draw(f, app, area);
}

fn draw_body(f: &mut Frame<'_>, app: &App, area: Rect) {
    if area.width >= WIDE {
        let [conn, mid, log] = Layout::horizontal([
            Constraint::Percentage(25),
            Constraint::Percentage(37),
            Constraint::Min(10),
        ])
        .areas(area);
        let [details, actions] =
            Layout::vertical([Constraint::Percentage(50), Constraint::Min(3)]).areas(mid);
        panes::connectors(f, app, conn);
        panes::details(f, app, details);
        panes::actions(f, app, actions);
        panes::log(f, app, log);
    } else {
        let rows = u16::try_from(app.snapshot.connectors.len()).unwrap_or(u16::MAX);
        let conn_h = (rows + 3).min(7).min(area.height / 3).max(3);
        let mid_h = (area.height / 3).clamp(3, 12);
        let [conn, mid, log] = Layout::vertical([
            Constraint::Length(conn_h),
            Constraint::Length(mid_h),
            Constraint::Min(3),
        ])
        .areas(area);
        panes::connectors(f, app, conn);
        if mid.width >= 60 {
            let [details, actions] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(mid);
            panes::details(f, app, details);
            panes::actions(f, app, actions);
        } else {
            panes::actions(f, app, mid);
        }
        panes::log(f, app, log);
    }
}

fn chip(text: &str, color: Color) -> Span<'static> {
    Span::styled(format!(" {text} "), Style::new().fg(Color::Black).bg(color))
}

fn draw_header(f: &mut Frame<'_>, app: &App, area: Rect) {
    use kirby_ocpp_core::ws::ConnectionState as C;
    let s = &app.snapshot;
    let (conn_text, conn_color) = match s.connection {
        C::Connected => ("connected".to_owned(), Color::Green),
        C::Connecting => ("connecting".to_owned(), Color::Yellow),
        C::Backoff { attempt } => (format!("retry #{attempt}"), Color::Yellow),
        C::Disconnected => ("disconnected".to_owned(), Color::Red),
    };
    let (reg_text, reg_color) = match s.registration {
        Some(r) if s.booted() => (r.to_string(), Color::Green),
        Some(r) => (r.to_string(), Color::Yellow),
        None => ("not booted".to_owned(), Color::DarkGray),
    };
    let w = usize::from(area.width);
    let conn_chip = chip(&conn_text, conn_color);
    let reg_chip = chip(&reg_text, reg_color);
    // A long id gives way (middle cut) so the connection state always stays visible.
    let id_room = w
        .saturating_sub(conn_chip.width() + reg_chip.width() + 3)
        .max(8);
    let mut spans = vec![
        Span::styled(
            format!(
                " {} ",
                crate::text::truncate_middle(&crate::text::sanitize(&s.charge_point_id), id_room)
            ),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        conn_chip,
        Span::raw(" "),
        reg_chip,
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    if w > used + 4 {
        spans.push(Span::styled(
            format!(
                "  {}",
                truncate(&crate::text::sanitize(&app.url), w - used - 2)
            ),
            dim(),
        ));
    }
    let conn_line = match s.connector(app.connector) {
        Some(c) => {
            let tx = c
                .tx_id()
                .map_or_else(|| "no tx".to_owned(), |t| format!("tx {t}"));
            format!(
                " connector {} | {} | {tx} | {} Wh",
                c.id, c.status, c.meter_wh
            )
        }
        None => " no connector".to_owned(),
    };
    let lines = vec![
        Line::from(spans),
        Line::styled(truncate(&conn_line, w), dim()),
        tab_line(app, w),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn tab_line(app: &App, w: usize) -> Line<'static> {
    let full: usize = Tab::ALL.iter().map(|t| width(t.title()) + 5).sum();
    let active = Style::new()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let spans: Vec<Span<'static>> = Tab::ALL
        .iter()
        .flat_map(|t| {
            let on = *t == app.tab;
            let label = if full <= w || on {
                format!(" {} {} ", t.index() + 1, t.title())
            } else {
                format!(" {} ", t.index() + 1)
            };
            [
                Span::styled(label, if on { active } else { Style::new() }),
                Span::raw(" "),
            ]
        })
        .collect();
    Line::from(spans)
}

fn draw_footer(f: &mut Frame<'_>, app: &App, area: Rect) {
    let w = usize::from(area.width);
    let first = if let Some(n) = &app.notice {
        let color = match n.level {
            NoticeLevel::Info => Color::Cyan,
            NoticeLevel::Success => Color::Green,
            NoticeLevel::Error => Color::Red,
        };
        Line::styled(truncate(&format!(" {}", n.text), w), Style::new().fg(color))
    } else if let Some(label) = app.pending.first() {
        let more = match app.pending.len() {
            0 | 1 => String::new(),
            n => format!(" (+{})", n - 1),
        };
        Line::styled(
            truncate(&format!(" waiting... {}{more}", sanitize(label)), w),
            Style::new().fg(Color::Yellow),
        )
    } else if let (Mode::Normal, Some(a)) = (&app.mode, app.selected_action()) {
        match app.reason_for(a.id) {
            Some(why) => Line::styled(
                truncate(&format!(" {}: disabled, {why}", a.label), w),
                Style::new().fg(Color::Yellow),
            ),
            None => Line::styled(truncate(&format!(" {}", a.desc), w), dim()),
        }
    } else {
        Line::raw("")
    };
    let line = if matches!(app.mode, Mode::Normal) {
        hint_line_pinned(&hints(app), PINNED_HINTS, w)
    } else {
        hint_line(&hints(app), w)
    };
    f.render_widget(Paragraph::new(vec![first, line]), area);
}

/// Always on the footer in normal mode; the last hints to go in a narrow terminal.
const PINNED_HINTS: &[(&str, &str)] = &[("?", "help"), (":", "palette")];

/// `(key, label)` pairs, most important first.
fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    use crate::app::Focus;
    match &app.mode {
        Mode::Normal => {
            let mut v = Vec::new();
            if app.focus == Focus::Log {
                v.extend([
                    ("Enter", "detail"),
                    ("y", "copy"),
                    ("f", "filter"),
                    ("s", "search"),
                    ("p", "pause"),
                ]);
            } else {
                v.push(("Enter", "run"));
            }
            v.extend([
                ("↑↓", "move"),
                ("←→", "pane"),
                ("1-7", "tab"),
                ("[ ]", "connector"),
                ("q", "quit"),
            ]);
            v
        }
        Mode::Palette(_) => vec![("Enter", "run"), ("Esc", "close"), ("↑↓", "select")],
        Mode::Form(_) => vec![
            ("Enter", "submit"),
            ("Esc", "cancel"),
            ("Tab", "next field"),
            ("←→", "choose/move"),
        ],
        Mode::Choice(_) => vec![("Enter", "select"), ("Esc", "cancel"), ("↑↓", "move")],
        Mode::Help => vec![("Esc", "close")],
        Mode::Detail(_) => vec![("Esc", "close"), ("↑↓", "scroll"), ("c", "copy")],
        Mode::Confirm(c) if c.action == ConfirmAction::Quit => {
            vec![("y/q", "quit"), ("n/Esc", "stay")]
        }
        Mode::Confirm(_) => vec![("y", "yes"), ("n", "no")],
        Mode::Browser(b) if b.filtering => {
            vec![("Enter", "keep"), ("Esc", "clear"), ("↑↓", "move")]
        }
        Mode::Browser(b) => {
            let mut v = match b.page {
                crate::browser::Page::Config => vec![("Enter", "edit")],
                crate::browser::Page::LocalList => {
                    vec![
                        ("a", "add"),
                        ("Enter", "edit"),
                        ("d", "remove"),
                        ("c", "clear"),
                    ]
                }
                crate::browser::Page::Certificates => Vec::new(),
                crate::browser::Page::Profiles => vec![
                    ("i", "install"),
                    ("e", "edit"),
                    ("Enter", "show"),
                    ("d", "delete"),
                    ("c", "clear"),
                ],
            };
            v.extend([
                ("/", "filter"),
                ("Tab", "page"),
                ("↑↓", "move"),
                ("Esc", "close"),
            ]);
            v
        }
    }
}

/// As many hints as fit in `w` cells; later ones (lower priority) are dropped, never clipped.
pub fn hint_line(hints: &[(&str, &str)], w: usize) -> Line<'static> {
    hint_line_pinned(hints, &[], w)
}

fn hint_cost(k: &str, label: &str) -> usize {
    width(k) + 1 + width(label) + 2
}

/// Like [`hint_line`], with `pinned` hints (most important first) that go last on the line and
/// are the last to be dropped: they are reserved room before the regular hints are placed.
pub fn hint_line_pinned(
    hints: &[(&str, &str)],
    pinned: &[(&str, &str)],
    w: usize,
) -> Line<'static> {
    let mut used = 1;
    let mut keep = 0;
    for (k, label) in pinned {
        let need = hint_cost(k, label);
        if used + need > w + 1 {
            break;
        }
        used += need;
        keep += 1;
    }
    let mut room = w + 1 - used.min(w + 1);
    let mut spans = vec![Span::raw(" ")];
    for (k, label) in hints {
        let need = hint_cost(k, label);
        if need > room {
            break;
        }
        room -= need;
        push_hint(&mut spans, k, label);
    }
    for (k, label) in pinned.iter().take(keep) {
        push_hint(&mut spans, k, label);
    }
    Line::from(spans)
}

fn push_hint(spans: &mut Vec<Span<'static>>, k: &str, label: &str) {
    spans.push(Span::styled(
        k.to_owned(),
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(format!(" {label}  "), dim()));
}

#[cfg(test)]
mod tests;
