//! Drawing. `draw` is a pure function of the [`App`]; it never mutates it.

mod panes;

use kirby_ocpp_tui::app::NoticeLevel;
use kirby_ocpp_tui::text::{sanitize, truncate};
use kirby_ocpp_tui::ui::{dim, hint_line};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::{App, Focus, Mode};
use crate::keys::HELP;

/// Width from which the list sits left of details and log.
pub const WIDE: u16 = 100;
pub const MIN_SIZE: (u16, u16) = (24, 8);

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
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(area);
    draw_header(f, app, header);
    draw_body(f, app, body);
    draw_footer(f, app, footer);
    overlay(f, app, area);
}

fn draw_body(f: &mut Frame<'_>, app: &App, area: Rect) {
    if area.width >= WIDE {
        let list_w = (area.width * 3 / 10).clamp(24, 44);
        let [list, right] =
            Layout::horizontal([Constraint::Length(list_w), Constraint::Min(10)]).areas(area);
        let [details, log] =
            Layout::vertical([Constraint::Percentage(55), Constraint::Min(3)]).areas(right);
        panes::stations(f, app, list);
        panes::details(f, app, details);
        panes::log(f, app, log);
    } else {
        let list_h = (area.height / 4).clamp(3, 8);
        let det_h = (area.height / 3).clamp(3, 14);
        let [list, details, log] = Layout::vertical([
            Constraint::Length(list_h),
            Constraint::Length(det_h),
            Constraint::Min(3),
        ])
        .areas(area);
        panes::stations(f, app, list);
        panes::details(f, app, details);
        panes::log(f, app, log);
    }
}

fn chip(text: &str, color: Color) -> Span<'static> {
    Span::styled(format!(" {text} "), Style::new().fg(Color::Black).bg(color))
}

fn draw_header(f: &mut Frame<'_>, app: &App, area: Rect) {
    let w = usize::from(area.width);
    let first = Line::from(vec![
        Span::styled(" kirby-csms ", Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(
            truncate(
                &format!("listening on {}", sanitize(&app.url)),
                w.saturating_sub(13),
            ),
            dim(),
        ),
    ]);
    let (conn, total) = (app.connected_count(), app.stations.len());
    let color = if conn > 0 {
        Color::Green
    } else {
        Color::DarkGray
    };
    let mut spans = vec![
        chip(&format!("{conn}/{total} connected"), color),
        Span::styled(
            format!(" frames in {} out {}", app.frames_in, app.frames_out),
            dim(),
        ),
    ];
    if app.stopped {
        spans.push(Span::raw(" "));
        spans.push(chip("server stopped", Color::Red));
    }
    f.render_widget(Paragraph::new(vec![first, Line::from(spans)]), area);
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
    } else {
        Line::raw("")
    };
    f.render_widget(Paragraph::new(vec![first, hint_line(&hints(app), w)]), area);
}

/// `(key, label)` pairs, most important first.
fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
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
            }
            v.extend([
                ("↑↓", "move"),
                ("←→", "pane"),
                ("/", "filter"),
                ("a", "all stations"),
                ("?", "help"),
                ("q", "quit"),
            ]);
            v
        }
        Mode::Input(_) => vec![("Enter", "keep"), ("Esc", "clear")],
        Mode::Help => vec![("Esc", "close")],
        Mode::Detail(_) => vec![("Esc", "close"), ("↑↓", "scroll"), ("c", "copy")],
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn popup(f: &mut Frame<'_>, rect: Rect, title: &str) -> Rect {
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Cyan))
        .title(title.to_owned());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

fn overlay(f: &mut Frame<'_>, app: &App, area: Rect) {
    match &app.mode {
        Mode::Normal => {}
        Mode::Input(input) => {
            let title = match input.target {
                crate::app::InputTarget::Stations => "Filter stations",
                crate::app::InputTarget::LogSearch => "Search log",
            };
            let rect = centered(area, 60.min(area.width.saturating_sub(2)), 3);
            let inner = popup(f, rect, title);
            let (shown, cursor) = input
                .text
                .window(usize::from(inner.width.saturating_sub(1)));
            f.render_widget(Paragraph::new(sanitize(&shown)), inner);
            let x = inner.x + u16::try_from(cursor).unwrap_or(0);
            f.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y));
        }
        Mode::Help => {
            let h = u16::try_from(HELP.len() + 2).unwrap_or(u16::MAX);
            let rect = centered(
                area,
                86.min(area.width.saturating_sub(2)),
                h.min(area.height),
            );
            let inner = popup(f, rect, "Help");
            let lines: Vec<Line<'_>> = HELP.iter().map(|l| Line::raw(*l)).collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
        Mode::Detail(d) => {
            let rect = centered(
                area,
                area.width.saturating_sub(4),
                area.height.saturating_sub(2),
            );
            let inner = popup(f, rect, &sanitize(&d.title));
            let lines: Vec<Line<'_>> = d
                .lines
                .iter()
                .skip(d.scroll)
                .take(usize::from(inner.height))
                .map(|l| Line::raw(truncate(l, usize::from(inner.width))))
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
