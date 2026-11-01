//! Drawing. `draw` is a pure function of the [`App`]; it never mutates it.

mod overlays;
mod panes;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::actions::Tab;
use crate::app::{App, Mode, NoticeLevel};
use crate::text::{truncate, width};

/// Width from which the three-column layout is used.
pub const WIDE: u16 = 110;
pub const MIN_SIZE: (u16, u16) = (24, 8);

pub fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

pub fn draw(f: &mut Frame<'_>, app: &App) {
    let area = f.area();
    if area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1 {
        let msg = format!(
            "terminal too small ({}x{}, need {}x{})",
            area.width, area.height, MIN_SIZE.0, MIN_SIZE.1
        );
        f.render_widget(
            Paragraph::new(truncate(&msg, usize::from(area.width))),
            area,
        );
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
    let mut spans = vec![
        Span::styled(
            format!(" {} ", crate::text::sanitize(&s.charge_point_id)),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        chip(&conn_text, conn_color),
        Span::raw(" "),
        chip(&reg_text, reg_color),
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
    f.render_widget(Paragraph::new(vec![first, hint_line(&hints(app), w)]), area);
}

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
                ("/", "palette"),
                ("[ ]", "connector"),
                ("?", "help"),
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
        Mode::Confirm(_) => vec![("y", "yes"), ("n", "no")],
    }
}

/// As many hints as fit in `w` cells; later ones (lower priority) are dropped, never clipped.
pub fn hint_line(hints: &[(&str, &str)], w: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1;
    for (k, label) in hints {
        let need = width(k) + 1 + width(label) + 2;
        if used + need > w + 1 {
            break;
        }
        spans.push(Span::styled(
            (*k).to_owned(),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}  "), dim()));
        used += need;
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests;
