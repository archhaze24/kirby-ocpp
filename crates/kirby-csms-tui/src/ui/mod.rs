//! Drawing. `draw` is a pure function of the [`App`]; it never mutates it.

mod overlays;
mod panes;

use kirby_ocpp_tui::app::NoticeLevel;
use kirby_ocpp_tui::text::{sanitize, truncate};
use kirby_ocpp_tui::theme::{Level, level_mark};
use kirby_ocpp_tui::ui::{PendingRow, dim, hint_line_pinned, pending_list};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::actions::ScopedRule;
use crate::app::{App, Focus, Mode};
use crate::keys::HELP;

/// Width from which the list sits left of details and log.
pub const WIDE: u16 = 100;
pub const MIN_SIZE: (u16, u16) = (24, 8);

pub fn draw(f: &mut Frame<'_>, app: &App) {
    draw_colored(f, app);
    if app.mono {
        kirby_ocpp_tui::theme::apply_monochrome(f.buffer_mut());
    }
}

fn draw_colored(f: &mut Frame<'_>, app: &App) {
    let area = f.area();
    if area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1 {
        kirby_ocpp_tui::ui::too_small(f, area, MIN_SIZE);
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

/// The URL a station connects to, from the listen URL: an unspecified listen host
/// (`0.0.0.0`, `[::]`) becomes `<this-host>`; `prefix` adds a path-prefix placeholder.
pub fn station_url_example(listen_url: &str, prefix: bool) -> String {
    let (scheme, rest) = listen_url.split_once("://").unwrap_or(("ws", listen_url));
    let rest = rest.trim_end_matches('/');
    let (host, port) = split_host_port(rest);
    let host = match host.as_str() {
        "0.0.0.0" | "[::]" | "" => "<this-host>".to_owned(),
        _ => host,
    };
    let port = if port.is_empty() {
        String::new()
    } else {
        format!(":{port}")
    };
    let prefix = if prefix { "/<prefix>" } else { "" };
    format!("{scheme}://{host}{port}{prefix}/<station id>")
}

/// Host (IPv6 in brackets) and port of `host:port`; the port is empty when there is none.
fn split_host_port(rest: &str) -> (String, String) {
    if let Ok(addr) = rest.parse::<std::net::SocketAddr>() {
        let host = match addr.ip() {
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        };
        return (host, addr.port().to_string());
    }
    if let Some(inner) = rest.strip_prefix('[')
        && let Some((address, after)) = inner.split_once(']')
    {
        let port = after.strip_prefix(':').unwrap_or_default();
        return (format!("[{address}]"), port.to_owned());
    }
    // A bare IPv6 address (several colons) has no port.
    if rest.matches(':').count() > 1 {
        return (format!("[{rest}]"), String::new());
    }
    match rest.rsplit_once(':') {
        Some((host, port)) => (host.to_owned(), port.to_owned()),
        None => (rest.to_owned(), String::new()),
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
    if app.security_profile > 0 {
        spans.push(Span::raw(" "));
        spans.push(chip(
            &format!("security profile {}", app.security_profile),
            Color::Blue,
        ));
    }
    if app.auth_failures > 0 {
        spans.push(Span::raw(" "));
        spans.push(chip(
            &format!(
                "{} auth failure{}",
                app.auth_failures,
                if app.auth_failures == 1 { "" } else { "s" }
            ),
            Color::Red,
        ));
    }
    if app.pending > 0 {
        spans.push(Span::raw(" "));
        spans.push(chip(&format!("{} pending", app.pending), Color::Yellow));
    }
    if !app.rules.is_empty() {
        // Global rules by action, a station's rules as `Action@station`.
        let actions = if app.rules.no_rules() {
            String::new()
        } else {
            let names: Vec<String> = app
                .rules
                .actions
                .iter()
                .cloned()
                .chain(app.rules.scoped.iter().map(ScopedRule::label))
                .collect();
            format!(" rules: {}", names.join(","))
        };
        spans.push(Span::styled(
            truncate(
                &format!(" faults {}{}", app.rules.faults, sanitize(&actions)),
                w.saturating_sub(40).max(10),
            ),
            Style::new().fg(Color::Magenta),
        ));
    }
    if app.stopped {
        spans.push(Span::raw(" "));
        spans.push(chip("server stopped", Color::Red));
    }
    f.render_widget(Paragraph::new(vec![first, Line::from(spans)]), area);
}

fn draw_footer(f: &mut Frame<'_>, app: &App, area: Rect) {
    let w = usize::from(area.width);
    let first = if let Some(n) = &app.notice {
        let (color, level) = match n.level {
            NoticeLevel::Info => (Color::Cyan, Level::Info),
            NoticeLevel::Success => (Color::Green, Level::Success),
            NoticeLevel::Error => (Color::Red, Level::Error),
        };
        let mark = if app.mono {
            format!(" {}", level_mark(level))
        } else {
            String::new()
        };
        Line::styled(
            truncate(&format!("{mark} {}", n.text), w),
            Style::new().fg(color),
        )
    } else {
        Line::raw("")
    };
    let line = if matches!(app.mode, Mode::Normal) {
        hint_line_pinned(&hints(app), PINNED_HINTS, w)
    } else {
        kirby_ocpp_tui::ui::hint_line(&hints(app), w)
    };
    f.render_widget(Paragraph::new(vec![first, line]), area);
}

/// Always on the footer in normal mode; the last hints to go in a narrow terminal.
const PINNED_HINTS: &[(&str, &str)] = &[("?", "help"), (":", "actions")];

/// `(key, label)` pairs, most important first.
fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    match &app.mode {
        Mode::Normal => {
            let mut v = Vec::new();
            if !app.inflight.is_empty() {
                v.push(("W", "pending"));
            }
            if app.focus == Focus::Log {
                v.extend([
                    ("Enter", "detail"),
                    ("y", "copy"),
                    ("f", "filter"),
                    ("s", "search"),
                    ("p", "pause"),
                ]);
            }
            v.extend([("↑↓", "move"), ("←→", "pane")]);
            // `/` narrows the focused pane; the log pane already lists `s` for the same thing.
            if app.focus == Focus::Stations {
                v.push(("/", "filter"));
            }
            v.extend([("a", "all stations"), ("q", "quit")]);
            v
        }
        Mode::Input(_) => vec![("Enter", "keep"), ("Esc", "clear")],
        Mode::Help => vec![("Esc", "close")],
        Mode::Detail(_) => vec![("Esc", "close"), ("↑↓", "scroll"), ("c", "copy")],
        Mode::Palette(_) => vec![("Enter", "run"), ("↑↓", "choose"), ("Esc", "close")],
        Mode::Form(_) => vec![
            ("Enter", "send"),
            ("Tab", "next field"),
            ("←→", "option"),
            ("Esc", "cancel"),
        ],
        Mode::Confirm(_) => vec![("y", "yes"), ("any other key", "cancel")],
        Mode::Results(_) => vec![("Enter", "open"), ("↑↓", "choose"), ("Esc", "close")],
        Mode::Pending(_) => vec![("x", "cancel"), ("↑↓", "choose"), ("Esc", "close")],
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
            let rect_w = kirby_ocpp_tui::ui::popup_width(area.width, 96);
            let wrapped =
                kirby_ocpp_tui::text::wrap_help(HELP, usize::from(rect_w.saturating_sub(2)));
            let mut wrapped = wrapped;
            if wrapped.len() + 2 > usize::from(area.height) {
                // A short terminal: drop the spacer lines so more of the help fits.
                wrapped.retain(|l| !l.is_empty());
            }
            let h = u16::try_from(wrapped.len() + 2).unwrap_or(u16::MAX);
            let rect = centered(area, rect_w, h.min(area.height));
            let inner = popup(f, rect, "Help");
            let lines: Vec<Line<'_>> = wrapped.into_iter().map(Line::from).collect();
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
        Mode::Palette(p) => overlays::palette(f, app, p, area),
        Mode::Form(form) => overlays::form(f, form, area),
        Mode::Confirm(c) => overlays::confirm(f, c, area),
        Mode::Results(list) => overlays::results(f, app, list, area),
        Mode::Pending(sel) => {
            let rows: Vec<PendingRow> = app
                .inflight
                .iter()
                .map(|c| PendingRow {
                    elapsed: kirby_ocpp_tui::app::elapsed_text(
                        (app.now - c.since).to_std().unwrap_or_default(),
                    ),
                    text: format!("{} {}", sanitize(&c.station), c.command.label()),
                    cancellable: true,
                })
                .collect();
            pending_list(f, area, &rows, *sel);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
