use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use kirby_ocpp_core::station::{Direction, LogLevel};
use kirby_ocpp_tui::logview::{FrameKind, LogEntry};
use kirby_ocpp_tui::text::{sanitize, truncate, truncate_middle, width};
use kirby_ocpp_tui::theme::{Level, level_mark};
use kirby_ocpp_tui::ui::dim;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Focus, LogView, Summary, filter_label};

pub(super) const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// A bordered pane. `bottom` is the scroll marker on the lower border, when rows are cut.
pub(super) fn block(title: String, bottom: Option<String>, focused: bool) -> Block<'static> {
    let block = Block::bordered()
        .border_style(kirby_ocpp_tui::theme::border(focused))
        .title(title);
    match bottom {
        Some(text) => block.title_bottom(Line::from(text).right_aligned()),
        None => block,
    }
}

/// `▲ 3 more`, `▼ 5 more` or both: what is cut above and below the rows shown. `None` when
/// nothing is cut.
pub(super) fn markers(above: usize, below: usize) -> Option<String> {
    let mut parts = Vec::new();
    if above > 0 {
        parts.push(format!("\u{25b2} {above} more"));
    }
    if below > 0 {
        parts.push(format!("\u{25bc} {below} more"));
    }
    (!parts.is_empty()).then(|| format!(" {} ", parts.join("  ")))
}

pub(super) fn status_color(status: &str) -> Color {
    match status {
        "Available" => Color::Green,
        "Charging" | "Occupied" => Color::Cyan,
        "Faulted" => Color::Red,
        "Unavailable" => kirby_ocpp_tui::theme::SECONDARY,
        _ => Color::Yellow,
    }
}

/// `5s`, `3m`, `2h`, `4d`.
pub fn age(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

pub(super) fn clock(ts: DateTime<Utc>) -> String {
    ts.format("%H:%M:%S").to_string()
}

fn pad(s: &str, w: usize) -> String {
    let used = width(s);
    format!("{s}{}", " ".repeat(w.saturating_sub(used)))
}

// ---- station list ---------------------------------------------------------------------

fn station_row(s: &Summary, inner_w: usize, selected: bool, mono: bool) -> Line<'static> {
    let (dot, color) = if s.replaced {
        // Monochrome: a half-filled dot tells a replaced connection from a live one.
        (if mono { "◐" } else { "●" }, Color::Yellow)
    } else if s.connected {
        ("●", Color::Green)
    } else {
        ("○", kirby_ocpp_tui::theme::SECONDARY)
    };
    let tx = if s.active_tx > 0 {
        format!(" tx{}", s.active_tx)
    } else {
        String::new()
    };
    let model = match (&s.vendor, &s.model) {
        (_, Some(m)) if inner_w >= 34 => format!(" {}", truncate(&sanitize(m), 14)),
        _ => String::new(),
    };
    let protocol = match s.protocol.as_deref() {
        Some("ocpp1.6") => " 1.6",
        Some("ocpp2.0.1") => " 2.0.1",
        Some("ocpp2.1") => " 2.1",
        _ => "",
    };
    let id_w = inner_w
        .saturating_sub(2 + width(&model) + width(&tx) + width(protocol))
        .max(1);
    // Middle cut: ids that share a long prefix still differ in the tail.
    let id = pad(&truncate_middle(&sanitize(&s.id), id_w), id_w);
    let text_style = if s.connected { Style::new() } else { dim() };
    // A transaction counter of a disconnected station is stale: draw it dim.
    let tx_style = if s.connected {
        Style::new().fg(Color::Cyan)
    } else {
        dim()
    };
    let line = Line::from(vec![
        Span::styled(format!("{dot} "), Style::new().fg(color)),
        Span::styled(id, text_style),
        Span::styled(model, dim()),
        Span::styled(protocol, dim()),
        Span::styled(tx, tx_style),
    ]);
    if selected { line.style(SELECTED) } else { line }
}

pub fn stations(f: &mut Frame<'_>, app: &App, area: Rect) {
    let h = usize::from(area.height.saturating_sub(2));
    let inner_w = usize::from(area.width.saturating_sub(2));
    let len = app.visible_len();
    let cursor = app.cursor();
    let top = cursor.map_or(0, |c| (c + 1).saturating_sub(h));
    let top = top.min(len.saturating_sub(h));
    let focused = app.focus == Focus::Stations;
    // Only the rows that fit are built: a thousand stations cost the same as ten.
    let lines: Vec<Line<'static>> = (top..len.min(top + h))
        .filter_map(|row| {
            app.visible_at(row)
                .map(|s| station_row(s, inner_w, Some(row) == cursor && focused, app.mono))
        })
        .collect();
    let shown = lines.len();
    let mut title = String::from("Stations");
    if !app.station_filter.is_empty() {
        let _ = write!(title, " /{}", sanitize(&app.station_filter));
    }
    let _ = write!(title, " {len}/{}", app.stations.len());
    let lines = if lines.is_empty() {
        vec![Line::styled(
            if app.stations.is_empty() {
                "waiting for stations"
            } else {
                "no match"
            },
            dim(),
        )]
    } else {
        lines
    };
    let bottom = markers(top, len.saturating_sub(top + shown));
    f.render_widget(
        Paragraph::new(lines).block(block(truncate(&title, inner_w.max(1)), bottom, focused)),
        area,
    );
}

/// What the details pane shows before any station has connected.
pub(super) fn empty_state(app: &App) -> Vec<Line<'static>> {
    let url = sanitize(&app.url);
    let mut lines = vec![
        Line::raw(format!("Listening on {url}")),
        Line::raw("Point a charge point at:"),
        Line::styled(
            format!("  {}", super::station_url_example(&url, false)),
            Style::new().fg(Color::Cyan),
        ),
        Line::styled(
            "A path prefix before the station id is accepted (ws://host:port/prefix/id).",
            dim(),
        ),
    ];
    if let Some(web) = &app.web_url {
        lines.push(Line::raw(format!("Web UI: {}", sanitize(web))));
    }
    if app.security_profile == 0 {
        lines.push(Line::styled(
            "Security profile 0: any peer can connect.",
            dim(),
        ));
    }
    lines
}

// ---- log ------------------------------------------------------------------------------

/// The summary of a frame row: the key fields of the payload, or the stored one when the frame
/// has no object payload (an error, a frame kept as cut text).
fn frame_summary(e: &LogEntry, frame: &serde_json::Value) -> String {
    super::logsum::summarize(frame).unwrap_or_else(|| e.summary.clone())
}

fn row_line(app: &App, e: &LogEntry, inner_w: usize, selected: bool) -> Line<'static> {
    let mut spans = Vec::new();
    let stamp = app.time.prefix(e.ts);
    if !stamp.is_empty() {
        spans.push(Span::styled(stamp, dim()));
    }
    if app.view == LogView::All {
        let id = app.entry_station(e).map_or_else(String::new, sanitize);
        spans.push(Span::styled(
            format!("{} ", pad(&truncate(&id, 14), 14)),
            Style::new().fg(Color::Yellow),
        ));
    }
    let (arrow, color, body) = match (e.frame(), e.station_line()) {
        (Some(fr), _) => {
            let tag = match e.kind {
                Some(FrameKind::Call) => "CALL",
                Some(FrameKind::Result) => "RES ",
                Some(FrameKind::Error) => "ERR ",
                _ => "RAW ",
            };
            let color = match (e.kind, fr.direction) {
                (Some(FrameKind::Error), _) => Color::Red,
                (_, Direction::Out) => Color::Blue,
                (_, Direction::In) => Color::Magenta,
            };
            let rtt = e.rtt_ms.map_or_else(String::new, |ms| format!("  {ms}ms"));
            let action = sanitize(fr.action.as_deref().unwrap_or("?"));
            (
                if fr.direction == Direction::Out {
                    "→"
                } else {
                    "←"
                },
                color,
                format!("{tag} {action} {}{rtt}", frame_summary(e, &fr.frame)),
            )
        }
        (_, Some(l)) => {
            let color = match l.level {
                LogLevel::Info => Color::Gray,
                LogLevel::Success => Color::Green,
                LogLevel::Warn => Color::Yellow,
                LogLevel::Error => Color::Red,
            };
            let mark = if app.mono {
                level_mark(match l.level {
                    LogLevel::Info => Level::Info,
                    LogLevel::Success => Level::Success,
                    LogLevel::Warn => Level::Warn,
                    LogLevel::Error => Level::Error,
                })
            } else {
                "●"
            };
            (mark, color, e.summary.clone())
        }
        _ => ("?", Color::Gray, String::new()),
    };
    let lead: usize = spans.iter().map(Span::width).sum::<usize>() + 2;
    spans.push(Span::styled(format!("{arrow} "), Style::new().fg(color)));
    spans.push(Span::styled(
        truncate(&body, inner_w.saturating_sub(lead)),
        Style::new().fg(color),
    ));
    let line = Line::from(spans);
    if selected { line.style(SELECTED) } else { line }
}

pub fn log(f: &mut Frame<'_>, app: &App, area: Rect) {
    let log = app.log();
    let visible = log.visible();
    let h = usize::from(area.height.saturating_sub(2));
    let inner_w = usize::from(area.width.saturating_sub(2));
    let sel = log.selected_index();
    let top = match sel {
        Some(s) if log.follow => (s + 1).saturating_sub(h),
        Some(s) => s.saturating_sub(h / 2).min(visible.len().saturating_sub(h)),
        None => 0,
    };
    let focused = app.focus == Focus::Log;
    let lines: Vec<Line<'static>> = visible
        .iter()
        .enumerate()
        .skip(top)
        .take(h)
        .map(|(i, e)| row_line(app, e, inner_w, Some(i) == sel && focused))
        .collect();
    let scope = match (app.view, app.selected.as_deref()) {
        (LogView::All, _) => "all stations".to_owned(),
        (LogView::Station, Some(id)) => sanitize(id),
        (LogView::Station, None) => "-".to_owned(),
    };
    let mut title = format!("Log: {scope} [filter: {}]", filter_label(log.filter));
    if !log.search.is_empty() {
        let _ = write!(title, " /{}", sanitize(&log.search));
    }
    let _ = write!(title, " {}/{}", visible.len(), log.len());
    if !log.follow {
        title.push_str(" PAUSED");
    }
    let bottom = markers(top, visible.len().saturating_sub(top + lines.len()));
    f.render_widget(
        Paragraph::new(lines).block(block(truncate(&title, inner_w.max(1)), bottom, focused)),
        area,
    );
}
