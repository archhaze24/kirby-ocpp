use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use kirby_csms_core::{StationInfo, StationState, TransactionStatus};
use kirby_ocpp_core::station::{Direction, LogLevel};
use kirby_ocpp_tui::logview::{FrameKind, LogEntry};
use kirby_ocpp_tui::text::{sanitize, truncate, width};
use kirby_ocpp_tui::ui::dim;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Row, Table};

use crate::app::{App, Focus, LogView, Summary, filter_label};

const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

fn block(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        dim()
    };
    Block::bordered().border_style(style).title(title)
}

fn status_color(status: &str) -> Color {
    match status {
        "Available" => Color::Green,
        "Charging" => Color::Cyan,
        "Faulted" => Color::Red,
        "Unavailable" => Color::DarkGray,
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

fn clock(ts: DateTime<Utc>) -> String {
    ts.format("%H:%M:%S").to_string()
}

fn pad(s: &str, w: usize) -> String {
    let used = width(s);
    format!("{s}{}", " ".repeat(w.saturating_sub(used)))
}

// ---- station list ---------------------------------------------------------------------

fn station_row(s: &Summary, inner_w: usize, selected: bool) -> Line<'static> {
    let (dot, color) = if s.replaced {
        ("●", Color::Yellow)
    } else if s.connected {
        ("●", Color::Green)
    } else {
        ("○", Color::DarkGray)
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
    let id_w = inner_w
        .saturating_sub(2 + width(&model) + width(&tx))
        .max(1);
    let id = pad(&truncate(&sanitize(&s.id), id_w), id_w);
    let text_style = if s.connected { Style::new() } else { dim() };
    let line = Line::from(vec![
        Span::styled(format!("{dot} "), Style::new().fg(color)),
        Span::styled(id, text_style),
        Span::styled(model, dim()),
        Span::styled(tx, Style::new().fg(Color::Cyan)),
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
                .map(|s| station_row(s, inner_w, Some(row) == cursor && focused))
        })
        .collect();
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
    f.render_widget(
        Paragraph::new(lines).block(block(truncate(&title, inner_w.max(1)), focused)),
        area,
    );
}

// ---- details --------------------------------------------------------------------------

pub fn details(f: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(id) = app.selected.as_deref() else {
        f.render_widget(
            Paragraph::new(Line::styled("no station selected", dim()))
                .block(block("Station".to_owned(), false)),
            area,
        );
        return;
    };
    let inner_w = usize::from(area.width.saturating_sub(2));
    let outer = block(
        truncate(&format!("Station {}", sanitize(id)), inner_w.max(1)),
        false,
    );
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let lines = info_lines(app, app.info.as_ref(), app.state.as_ref(), inner_w);
    let info_h = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .min(inner.height);
    let [info, tables] =
        Layout::vertical([Constraint::Length(info_h), Constraint::Min(0)]).areas(inner);
    f.render_widget(Paragraph::new(lines), info);
    let Some(state) = &app.state else {
        return;
    };
    if tables.width >= 140 {
        let [c, t] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .areas(tables);
        connectors(f, app, state, c);
        transactions(f, app, state, t);
    } else {
        // Stacked: the connectors need a header and one row each, at most a third of the room.
        let wanted = u16::try_from(state.connectors.len() + 1).unwrap_or(u16::MAX);
        let conn_h = wanted.clamp(2, (tables.height / 3).max(2));
        let [c, t] =
            Layout::vertical([Constraint::Length(conn_h), Constraint::Min(0)]).areas(tables);
        connectors(f, app, state, c);
        transactions(f, app, state, t);
    }
}

fn info_lines(
    app: &App,
    info: Option<&StationInfo>,
    state: Option<&StationState>,
    w: usize,
) -> Vec<Line<'static>> {
    let now = app.now;
    let mut out: Vec<String> = Vec::new();
    if let Some(i) = info {
        let link = if i.connected {
            let since = i
                .connected_at
                .map_or_else(String::new, |t| format!(" for {}", age(now, t)));
            format!("connected{since}")
        } else {
            let since = i
                .disconnected_at
                .map_or_else(String::new, |t| format!(" {} ago", age(now, t)));
            format!("disconnected{since}")
        };
        let addr = i
            .remote_addr
            .map_or_else(|| "-".to_owned(), |a| a.to_string());
        let proto = i.subprotocol.as_deref().unwrap_or("none");
        out.push(format!("{link}  remote {addr}  subprotocol {proto}"));
    }
    match state.and_then(|s| s.boot.as_ref()) {
        Some(b) => {
            let mut line = format!("boot {} {}", b.vendor, b.model);
            if let Some(fw) = &b.firmware_version {
                let _ = write!(line, "  fw {fw}");
            }
            if let Some(sn) = b
                .charge_point_serial_number
                .as_ref()
                .or(b.charge_box_serial_number.as_ref())
            {
                let _ = write!(line, "  sn {sn}");
            }
            if let Some(r) = b.registration {
                let _ = write!(line, "  answered {r}");
            }
            if let Some(iv) = b.interval {
                let _ = write!(line, " every {iv}s");
            }
            out.push(line);
        }
        None => out.push("not booted".to_owned()),
    }
    if let Some(s) = state {
        let hb = s
            .last_heartbeat_at
            .map_or_else(|| "none".to_owned(), |t| format!("{} ago", age(now, t)));
        let mut line = format!("heartbeat {hb}");
        if let Some(fw) = &s.firmware_status {
            let _ = write!(line, "  firmware {} ({} ago)", fw.status, age(now, fw.at));
        }
        if let Some(d) = &s.diagnostics_status {
            let _ = write!(line, "  diagnostics {} ({} ago)", d.status, age(now, d.at));
        }
        out.push(line);
        let received = info.map_or(0, |i| i.received_calls);
        out.push(format!(
            "calls received {received}  failed {}  unknown stops {}",
            s.failed_calls, s.unknown_stops
        ));
        out.extend(security_lines(app, s));
    }
    out.into_iter()
        .map(|l| Line::raw(truncate(&sanitize(&l), w)))
        .collect()
}

/// What the station reported under the Security Whitepaper: nothing when it reported nothing.
fn security_lines(app: &App, s: &StationState) -> Vec<String> {
    let mut parts = Vec::new();
    if s.security_events_total > 0 {
        parts.push(format!("security events {}", s.security_events_total));
    }
    if let Some(l) = &s.log_status {
        let id = l.request_id.map_or_else(String::new, |i| format!(" #{i}"));
        parts.push(format!("log {}{id} ({} ago)", l.status, age(app.now, l.at)));
    }
    if let Some(f) = &s.signed_firmware_status {
        let id = f.request_id.map_or_else(String::new, |i| format!(" #{i}"));
        parts.push(format!(
            "signed firmware {}{id} ({} ago)",
            f.status,
            age(app.now, f.at)
        ));
    }
    if let Some(r) = &s.certificate_request {
        parts.push(format!(
            "CSR received ({} ago)",
            age(app.now, r.received_at)
        ));
    }
    let mut out = Vec::new();
    if !parts.is_empty() {
        out.push(parts.join("  "));
    }
    // The last three events, newest first (`e` lists them all).
    for e in s.security_events.iter().rev().take(3) {
        let info = e
            .tech_info
            .as_deref()
            .map_or_else(String::new, |t| format!(" {t}"));
        out.push(format!(
            "  {} {}{info}",
            e.received_at.format("%H:%M:%S"),
            e.event_type
        ));
    }
    out
}

fn connectors(f: &mut Frame<'_>, app: &App, state: &StationState, area: Rect) {
    let rows = usize::from(area.height.saturating_sub(1));
    let table = Table::new(
        state.connectors.iter().take(rows).map(|(id, c)| {
            let status = c.status.to_string();
            let color = status_color(&status);
            let error = c.error_code.to_string();
            Row::new(vec![
                id.to_string(),
                status,
                error,
                c.info.as_deref().map(sanitize).unwrap_or_default(),
                age(app.now, c.received_at),
            ])
            .style(Style::new().fg(color))
        }),
        [
            Constraint::Length(4),
            Constraint::Length(13),
            Constraint::Length(12),
            Constraint::Min(4),
            Constraint::Length(5),
        ],
    )
    .header(Row::new(["#", "Status", "Error", "Info", "Upd"]).style(dim()));
    f.render_widget(table, area);
}

fn transactions(f: &mut Frame<'_>, app: &App, state: &StationState, area: Rect) {
    let rows = usize::from(area.height.saturating_sub(1));
    // Newest first: that is what one looks for.
    let table = Table::new(
        state.transactions.iter().rev().take(rows).map(|t| {
            let active = t.status == TransactionStatus::Active;
            let meters = match t.meter_stop {
                Some(stop) => format!("{}→{stop}", t.meter_start),
                None => t.meter_start.to_string(),
            };
            let wh = t
                .energy_wh
                .map_or_else(|| "-".to_owned(), |v| format!("{v:.0}"));
            let w = t
                .power_w
                .map_or_else(|| "-".to_owned(), |v| format!("{v:.0}"));
            let when = match t.stopped_at {
                Some(stop) => format!("{}-{}", clock(t.started_at), clock(stop)),
                None => format!("{} ({})", clock(t.started_at), age(app.now, t.started_at)),
            };
            Row::new(vec![
                t.id.to_string(),
                t.connector.to_string(),
                sanitize(&t.id_tag),
                meters,
                wh,
                w,
                if active { "active" } else { "stopped" }.to_owned(),
                when,
            ])
            .style(if active {
                Style::new().fg(Color::Cyan)
            } else {
                dim()
            })
        }),
        [
            Constraint::Length(6),
            Constraint::Length(3),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(7),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Min(8),
        ],
    )
    .header(Row::new(["Tx", "C", "idTag", "Meter", "Wh", "W", "State", "Time"]).style(dim()));
    f.render_widget(table, area);
}

// ---- log ------------------------------------------------------------------------------

fn row_line(app: &App, e: &LogEntry, inner_w: usize, selected: bool) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{} ", e.ts.format("%H:%M:%S%.3f")),
        dim(),
    )];
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
                format!("{tag} {action} {}{rtt}", e.summary),
            )
        }
        (_, Some(l)) => {
            let color = match l.level {
                LogLevel::Info => Color::Gray,
                LogLevel::Success => Color::Green,
                LogLevel::Warn => Color::Yellow,
                LogLevel::Error => Color::Red,
            };
            ("●", color, e.summary.clone())
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
    let mut title = format!("Log {scope} [{}]", filter_label(log.filter));
    if !log.search.is_empty() {
        let _ = write!(title, " /{}", sanitize(&log.search));
    }
    let _ = write!(title, " {}/{}", visible.len(), log.len());
    if !log.follow {
        title.push_str(" PAUSED");
    }
    f.render_widget(
        Paragraph::new(lines).block(block(truncate(&title, inner_w.max(1)), focused)),
        area,
    );
}
