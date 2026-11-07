use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use kirby_ocpp_core::ocpp::types::ConnectorStatus;
use kirby_ocpp_core::station::{ConnectorSnapshot, Direction, TransactionState};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Row, Table, TableState};

use super::dim;
use crate::actions::Tab;
use crate::app::{App, Focus, StepState};
use crate::logview::{FrameKind, LogEntry};
use crate::text::{sanitize, truncate};

fn block(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        dim()
    };
    Block::bordered().border_style(style).title(title)
}

fn status_color(s: ConnectorStatus) -> Color {
    match s {
        ConnectorStatus::Available => Color::Green,
        ConnectorStatus::Charging => Color::Cyan,
        ConnectorStatus::Faulted => Color::Red,
        ConnectorStatus::Unavailable => Color::DarkGray,
        _ => Color::Yellow,
    }
}

const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// Table columns in priority order: header, width. Columns that do not fit are dropped
/// from the right, so the status is never truncated.
const CONNECTOR_COLS: [(&str, u16); 5] =
    [("#", 2), ("Status", 13), ("Tx", 10), ("Wh", 9), ("Res", 4)];

fn connector_cols(inner_w: u16) -> usize {
    let mut used = 0u16;
    let mut n = 0;
    for (i, (_, w)) in CONNECTOR_COLS.iter().enumerate() {
        used += w + u16::from(i > 0);
        if used > inner_w && n >= 2 {
            break;
        }
        n += 1;
    }
    n
}

pub fn connectors(f: &mut Frame<'_>, app: &App, area: Rect) {
    let n = connector_cols(area.width.saturating_sub(2));
    let rows: Vec<Row<'_>> = app
        .snapshot
        .connectors
        .iter()
        .map(|c| {
            let cells = vec![
                Span::raw(c.id.to_string()),
                Span::styled(
                    c.status.to_string(),
                    Style::new().fg(status_color(c.status)),
                ),
                Span::raw(c.tx_id().map_or_else(|| "-".to_owned(), |t| t.to_string())),
                Span::raw(c.meter_wh.to_string()),
                Span::raw(
                    c.reservation
                        .as_ref()
                        .map_or_else(|| "-".to_owned(), |r| r.id.to_string()),
                ),
            ];
            Row::new(cells.into_iter().take(n))
        })
        .collect();
    let widths = CONNECTOR_COLS
        .iter()
        .take(n)
        .map(|&(_, w)| Constraint::Length(w));
    let table = Table::new(rows, widths)
        .header(Row::new(CONNECTOR_COLS.iter().take(n).map(|c| c.0)).style(dim()))
        .row_highlight_style(SELECTED)
        .block(block(
            "Connectors".to_owned(),
            app.focus == Focus::Connectors,
        ));
    let mut state = TableState::default().with_selected(
        app.snapshot
            .connectors
            .iter()
            .position(|c| c.id == app.connector),
    );
    f.render_stateful_widget(table, area, &mut state);
}

fn kv(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<16}"), dim()),
        Span::raw(v.into()),
    ])
}

fn when(t: DateTime<Utc>) -> String {
    t.format("%H:%M:%S").to_string()
}

pub fn details(f: &mut Frame<'_>, app: &App, area: Rect) {
    let lines = match app.tab {
        Tab::Scenarios => scenario_lines(app),
        Tab::Station | Tab::Maintenance | Tab::Data => station_lines(app),
        _ => app
            .snapshot
            .connector(app.connector)
            .map(connector_lines)
            .unwrap_or_default(),
    };
    let title = match app.tab {
        Tab::Scenarios => "Scenario".to_owned(),
        Tab::Station | Tab::Maintenance | Tab::Data => "Station".to_owned(),
        _ => format!("Connector {}", app.connector),
    };
    f.render_widget(Paragraph::new(lines).block(block(title, false)), area);
}

fn station_lines(app: &App) -> Vec<Line<'static>> {
    let s = &app.snapshot;
    vec![
        kv(
            "registration",
            s.registration
                .map_or_else(|| "not booted".to_owned(), |r| r.to_string()),
        ),
        kv(
            "heartbeat",
            format!(
                "every {}s, last {}",
                s.heartbeat_interval_secs,
                s.last_heartbeat_at.map_or_else(|| "-".to_owned(), when)
            ),
        ),
        kv("availability", s.availability.to_string()),
        kv("local list", format!("version {}", s.local_list_version)),
        kv(
            "diagnostics",
            format!(
                "{} (next: {:?})",
                s.diagnostics_status, s.diagnostics_outcome
            ),
        ),
        kv(
            "firmware",
            format!("{} (next: {:?})", s.firmware_status, s.firmware_outcome),
        ),
        kv(
            "security",
            format!(
                "profile {}, {} cert(s)",
                s.security_profile,
                s.certificates.len()
            ),
        ),
        kv(
            "sec. events",
            format!(
                "{} queued{}",
                s.pending_security_events,
                if s.certificate_request_pending {
                    ", CSR pending"
                } else {
                    ""
                }
            ),
        ),
        kv(
            "log upload",
            format!(
                "{}{}",
                s.log_status,
                s.log_request_id
                    .map_or_else(String::new, |id| format!(" (request {id})"))
            ),
        ),
        kv(
            "signed firmware",
            format!(
                "{}{}",
                s.signed_firmware_status,
                s.signed_firmware_request_id
                    .map_or_else(String::new, |id| format!(" (request {id})"))
            ),
        ),
        kv(
            "queued",
            format!("{} offline message(s)", s.pending_offline_messages),
        ),
        kv(
            "rules",
            if s.response_rules.is_empty() {
                "none".to_owned()
            } else {
                s.response_rules
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ),
    ]
}

fn connector_lines(c: &ConnectorSnapshot) -> Vec<Line<'static>> {
    let mut v = vec![
        kv("status", c.status.to_string()),
        kv(
            "error",
            format!(
                "{}{}",
                c.error_code,
                c.info
                    .as_deref()
                    .map_or_else(String::new, |i| format!(" ({})", sanitize(i)))
            ),
        ),
        kv(
            "EV",
            if c.ev_connected {
                "plugged in"
            } else {
                "not connected"
            },
        ),
        kv("availability", c.availability.to_string()),
        kv("meter", format!("{} Wh", c.meter_wh)),
    ];
    v.push(match &c.transaction {
        TransactionState::Idle => kv("transaction", "none"),
        TransactionState::PendingStart(t) => kv(
            "transaction",
            format!("starting ({}) tag {}", t.tx_id, t.id_tag),
        ),
        TransactionState::Active(t) => kv(
            "transaction",
            format!(
                "{} tag {} since {}, {} Wh",
                t.tx_id,
                t.id_tag,
                when(t.started_at),
                c.meter_wh.saturating_sub(t.meter_start_wh)
            ),
        ),
        TransactionState::Stopping(t) => kv(
            "transaction",
            format!("stopping ({}, {})", t.tx_id, t.reason),
        ),
    });
    v.push(kv(
        "reservation",
        c.reservation.as_ref().map_or_else(
            || "none".to_owned(),
            |r| {
                format!(
                    "#{} tag {} until {}",
                    r.id,
                    r.id_tag,
                    r.expiry.format("%H:%M:%S")
                )
            },
        ),
    ));
    v.push(kv(
        "last tag",
        c.last_id_tag
            .as_ref()
            .map_or_else(|| "-".to_owned(), ToString::to_string),
    ));
    v
}

fn scenario_lines(app: &App) -> Vec<Line<'static>> {
    let sc = &app.scenario;
    if sc.name.is_empty() {
        return vec![Line::styled(
            "No scenario run yet. Pick one in the actions list.",
            dim(),
        )];
    }
    let state = match (&sc.report, sc.running) {
        (_, true) => "running".to_owned(),
        (Some(r), _) if r.passed => format!("passed in {} ms", r.duration_ms),
        (Some(r), _) if r.cancelled => "cancelled".to_owned(),
        (Some(_), _) => "FAILED".to_owned(),
        (None, false) => "not started".to_owned(),
    };
    let mut v = vec![Line::from(vec![
        Span::styled(sc.name.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {state}")),
    ])];
    for (i, (name, st)) in sc.steps.iter().enumerate() {
        let (mark, color, extra) = match st {
            StepState::Pending => (" ", Color::DarkGray, String::new()),
            StepState::Running => (">", Color::Cyan, String::new()),
            StepState::Passed(ms) => ("+", Color::Green, format!(" ({ms} ms)")),
            StepState::Failed(why) => (
                "x",
                Color::Red,
                format!(
                    ": {}",
                    sanitize(crate::text::without_step_prefix(name, why))
                ),
            ),
            StepState::Skipped => ("-", Color::DarkGray, " skipped".to_owned()),
        };
        v.push(Line::styled(
            format!("{mark} {}. {}{extra}", i + 1, sanitize(name)),
            Style::new().fg(color),
        ));
    }
    v
}

pub fn actions(f: &mut Frame<'_>, app: &App, area: Rect) {
    let idx = app.tab_actions();
    let items: Vec<ListItem<'_>> = idx
        .iter()
        .map(|&i| {
            let a = &app.catalog[i];
            let key = a.key.map_or_else(|| "  ".to_owned(), |k| format!("{k} "));
            let style = if app.reason_for(a.id).is_some() {
                dim()
            } else {
                Style::new()
            };
            ListItem::new(Line::from(vec![
                Span::styled(key, Style::new().fg(Color::Cyan)),
                Span::styled(a.label.clone(), style),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.action_sel[app.tab.index()]));
    let list = List::new(items).highlight_style(SELECTED).block(block(
        format!("Actions: {}", app.tab.title()),
        app.focus == Focus::Actions,
    ));
    f.render_stateful_widget(list, area, &mut state);
}

fn row_line(e: &LogEntry, inner_w: usize, selected: bool) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{} ", e.ts.format("%H:%M:%S%.3f")),
        dim(),
    )];
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
            use kirby_ocpp_core::station::LogLevel as L;
            let color = match l.level {
                L::Info => Color::Gray,
                L::Success => Color::Green,
                L::Warn => Color::Yellow,
                L::Error => Color::Red,
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
    let log = &app.log;
    let visible = log.visible();
    let h = usize::from(area.height.saturating_sub(2));
    let inner_w = usize::from(area.width.saturating_sub(2));
    let sel = log.selected_index();
    let top = match sel {
        Some(s) if log.follow => (s + 1).saturating_sub(h),
        Some(s) => s.saturating_sub(h / 2).min(visible.len().saturating_sub(h)),
        None => 0,
    };
    let lines: Vec<Line<'static>> = visible
        .iter()
        .enumerate()
        .skip(top)
        .take(h)
        .map(|(i, e)| row_line(e, inner_w, Some(i) == sel && app.focus == Focus::Log))
        .collect();
    let mut title = format!("Log [{}]", log.filter.label());
    if !log.search.is_empty() {
        let _ = write!(title, " /{}", sanitize(&log.search));
    }
    let _ = write!(title, " {}/{}", visible.len(), log.len());
    if !log.follow {
        title.push_str(" PAUSED");
    }
    f.render_widget(
        Paragraph::new(lines).block(block(
            truncate(&title, inner_w.max(1)),
            app.focus == Focus::Log,
        )),
        area,
    );
}
