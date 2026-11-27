//! The panes of the OCPP 2.x UI: EVSE list, details, actions and log. Every string that comes
//! from the station or the CSMS is sanitized before it is drawn.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use kirby_ocpp_core::ocpp::v2::types::WireEnum;
use kirby_ocpp_core::station::{Direction, LogLevel};
use kirby_ocpp_core::station2::{EvseSnapshot, TransactionSnapshot};
use kirby_ocpp_core::timefmt::TimeMode;
use kirby_ocpp_core::ws::ConnectionState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Row, Table, TableState};

use super::actions::Tab2;
use super::app::App2;
use crate::app::{Focus, StepState};
use crate::logview::{FrameKind, LogEntry};
use crate::text::{sanitize, truncate};
use crate::theme::{Level, level_mark};
use crate::ui::overlays::SELECTED;
use crate::ui::{count_of, dim, scrolled_text, window_start, with_markers, wrap_lines};

/// Characters of a transaction id that the list row shows.
const SHORT_TX: usize = 8;

fn block(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        dim()
    };
    Block::bordered().border_style(style).title(title)
}

/// Color of a connector status in its wire spelling.
fn status_color(status: &str) -> Color {
    match status {
        "Available" => Color::Green,
        "Occupied" => Color::Cyan,
        "Reserved" => Color::Magenta,
        "Unavailable" => Color::Yellow,
        "Faulted" => Color::Red,
        _ => Color::Gray,
    }
}

/// `Operative` or `Inoperative`.
fn operative_text(operative: bool) -> &'static str {
    if operative {
        "Operative"
    } else {
        "Inoperative"
    }
}

/// The connection state as the header chip and the details write it.
pub(super) fn connection_text(state: ConnectionState) -> (String, Color) {
    match state {
        ConnectionState::Connected => ("connected".to_owned(), Color::Green),
        ConnectionState::Connecting => ("connecting".to_owned(), Color::Yellow),
        ConnectionState::Backoff { attempt } => (format!("retry #{attempt}"), Color::Yellow),
        ConnectionState::Disconnected => ("disconnected".to_owned(), Color::Red),
    }
}

/// Table columns in priority order: header, width. Columns that do not fit are dropped from
/// the right, so the status is never truncated.
const EVSE_COLS: [(&str, u16); 6] = [
    ("#", 4),
    ("Status", 11),
    ("EV", 2),
    ("Tx", 8),
    ("Wh", 8),
    ("Op", 11),
];

fn evse_cols(inner_w: u16) -> usize {
    let mut used = 0u16;
    let mut n = 0;
    for (i, (_, w)) in EVSE_COLS.iter().enumerate() {
        used += w + u16::from(i > 0);
        if used > inner_w && n >= 2 {
            break;
        }
        n += 1;
    }
    n
}

/// Rows of the EVSE list and the row of the selected EVSE. An EVSE with several connectors
/// gets one sub-row per further connector.
fn evse_rows(app: &App2, n: usize) -> (Vec<Row<'static>>, Option<usize>) {
    let mut rows = Vec::new();
    let mut selected = None;
    for evse in &app.snapshot.evses {
        if evse.id == app.evse {
            selected = Some(rows.len());
        }
        let tx = evse.transaction.as_ref().map_or_else(
            || "-".to_owned(),
            |t| sanitize(&t.transaction_id).chars().take(SHORT_TX).collect(),
        );
        let first_extra = |evse: &EvseSnapshot| {
            [
                Span::raw(tx.clone()),
                Span::raw(evse.meter_wh.to_string()),
                Span::raw(operative_text(evse.operative)),
            ]
        };
        if evse.connectors.is_empty() {
            let [tx, wh, op] = first_extra(evse);
            rows.push(Row::new(
                [
                    Span::raw(evse.id.to_string()),
                    Span::raw("-"),
                    Span::raw("-"),
                    tx,
                    wh,
                    op,
                ]
                .into_iter()
                .take(n),
            ));
            continue;
        }
        for (i, c) in evse.connectors.iter().enumerate() {
            let status = c.status.as_str();
            let id = if i == 0 {
                evse.id.to_string()
            } else {
                format!("{}.{}", evse.id, c.id)
            };
            let mut cells = vec![
                Span::raw(id),
                Span::styled(status, Style::new().fg(status_color(status))),
                Span::raw(if c.ev_connected { "P" } else { "-" }),
            ];
            if i == 0 {
                cells.extend(first_extra(evse));
            }
            rows.push(Row::new(cells.into_iter().take(n)));
        }
    }
    (rows, selected)
}

pub(super) fn evses(f: &mut Frame<'_>, app: &App2, area: Rect) {
    let n = evse_cols(area.width.saturating_sub(2));
    let (rows, selected) = evse_rows(app, n);
    let total = rows.len();
    // Borders and the header row take three rows; the rest is the window over the list.
    let room = usize::from(area.height.saturating_sub(3));
    let start = window_start(selected.unwrap_or(0), room, total);
    let rows: Vec<Row<'static>> = rows.into_iter().skip(start).take(room).collect();
    let widths = EVSE_COLS
        .iter()
        .take(n)
        .map(|&(_, w)| Constraint::Length(w));
    let evse_no = app
        .snapshot
        .evses
        .iter()
        .position(|e| e.id == app.evse)
        .unwrap_or(0);
    let table = Table::new(rows, widths)
        .header(Row::new(EVSE_COLS.iter().take(n).map(|c| c.0)).style(dim()))
        .row_highlight_style(SELECTED)
        .block(with_markers(
            block(
                // The count is of EVSEs; the rows can be more (one per extra connector).
                format!(
                    "EVSEs{}",
                    if total > room {
                        format!(" {}/{}", evse_no + 1, app.snapshot.evses.len())
                    } else {
                        String::new()
                    }
                ),
                app.focus == Focus::Connectors,
            ),
            start,
            total.saturating_sub(start + room),
        ));
    let mut state = TableState::default().with_selected(selected.map(|s| s - start));
    f.render_stateful_widget(table, area, &mut state);
}

fn kv(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<17}"), dim()),
        Span::raw(truncate(&v.into(), 120)),
    ])
}

fn when(t: DateTime<Utc>) -> String {
    t.format("%H:%M:%S").to_string()
}

pub(super) fn details(f: &mut Frame<'_>, app: &App2, area: Rect) {
    let (title, lines) = match app.tab {
        Tab2::Scenarios => ("Scenario".to_owned(), scenario_lines(app)),
        Tab2::Station | Tab2::Data | Tab2::Logs => ("Station".to_owned(), station_lines(app)),
        Tab2::Evse => (format!("EVSE {}", app.evse), evse_lines(app, false)),
        Tab2::Transaction => (format!("EVSE {}", app.evse), evse_lines(app, true)),
    };
    let lines = wrap_lines(lines, usize::from(area.width.saturating_sub(2)));
    scrolled_text(
        f,
        area,
        block(title, app.focus == Focus::Details),
        lines,
        app.details.scroll,
        &app.details.max,
    );
}

fn station_lines(app: &App2) -> Vec<Line<'static>> {
    let s = &app.snapshot;
    let mut v = vec![
        kv("connection", connection_text(s.connection).0),
        kv(
            "registration",
            s.registration
                .map_or_else(|| "not booted".to_owned(), |r| r.as_str().to_owned()),
        ),
        kv(
            "boot reason",
            s.boot_reason
                .map_or_else(|| "-".to_owned(), |r| r.as_str().to_owned()),
        ),
        kv(
            "heartbeat",
            format!(
                "every {} s, last {}",
                s.heartbeat_interval_secs,
                s.last_heartbeat_at.map_or_else(|| "-".to_owned(), when)
            ),
        ),
        kv("availability", operative_text(s.availability_operative)),
        kv("security profile", s.security_profile.to_string()),
        kv("queued events", s.queued_events.to_string()),
        kv(
            "local list",
            format!(
                "v{}, {} entries",
                s.local_list_version, s.local_list_entries
            ),
        ),
        kv(
            "profiles",
            format!("{} charging profiles", s.charging_profiles),
        ),
        kv(
            "security events",
            format!("{} sent (since UI start)", app.security_events_sent),
        ),
    ];
    if let Some(why) = &s.reconnect_stopped {
        v.push(kv("reconnect", format!("stopped: {}", sanitize(why))));
    }
    v.push(kv("vendor", sanitize(&app.vendor)));
    v
}

fn transaction_lines(tx: &TransactionSnapshot) -> Vec<Line<'static>> {
    vec![
        kv("transaction", "running"),
        // On its own line: the full id (36 characters) needs the width of the pane.
        Line::from(Span::raw(sanitize(&tx.transaction_id))),
        kv("charging state", tx.charging_state.as_str()),
        kv(
            "idToken",
            tx.id_token
                .as_deref()
                .map_or_else(|| "-".to_owned(), sanitize),
        ),
        kv("started", when(tx.started_at)),
        kv("energy", format!("{} Wh", tx.energy_wh)),
        kv(
            "remoteStartId",
            tx.remote_start_id
                .map_or_else(|| "-".to_owned(), |id| id.to_string()),
        ),
        kv("next seqNo", tx.next_seq_no.to_string()),
    ]
}

/// The OCPP 2.1 limit, cost, tariff and priority rows of an EVSE; each only with content.
fn evse_extra_lines(evse: &EvseSnapshot) -> Vec<Line<'static>> {
    let mut v = Vec::new();
    // The currency of the tariff in force, shown after every cost; none known: the number alone.
    let currency = evse
        .cost_currency
        .as_deref()
        .map(|c| truncate(&sanitize(c.trim()), 8))
        .filter(|c| !c.is_empty());
    let money = |value: f64| match &currency {
        Some(c) => format!("{value:.2} {c}"),
        None => format!("{value:.2}"),
    };
    // One line per limit kind: the details pane is narrow and does not wrap.
    if let Some(limit) = &evse.transaction_limit {
        if let Some(wh) = limit.max_energy_wh {
            v.push(kv("max energy", format!("{:.1} kWh", wh / 1000.0)));
        }
        if let Some(s) = limit.max_time_s {
            let time = if s % 60 == 0 {
                format!("{} min", s / 60)
            } else {
                format!("{s} s")
            };
            v.push(kv("max time", time));
        }
        if let Some(cost) = limit.max_cost {
            v.push(kv("max cost", money(cost)));
        }
        if let Some(reached) = &evse.limit_reached {
            v.push(Line::from(vec![
                Span::styled(format!("{:<17}", "limit reached"), dim()),
                Span::styled(sanitize(reached), Style::new().fg(Color::Yellow)),
            ]));
        }
    }
    let tariff = evse.tariff_id.as_deref().map(sanitize);
    match (evse.total_cost, &tariff) {
        (Some(cost), Some(id)) => v.push(kv("cost", format!("{} \u{b7} tariff {id}", money(cost)))),
        (Some(cost), None) => v.push(kv("cost", money(cost))),
        (None, Some(id)) => v.push(kv("tariff", id.clone())),
        (None, None) => {}
    }
    if evse.priority_charging {
        v.push(Line::from(Span::raw("priority charging")));
    }
    if tariff.is_none()
        && let Some(id) = &evse.default_tariff_id
    {
        v.push(kv("default tariff", sanitize(id)));
    }
    v
}

fn evse_lines(app: &App2, transaction_first: bool) -> Vec<Line<'static>> {
    let Some(evse) = app.selected_evse() else {
        return vec![Line::styled("The station has no EVSE.", dim())];
    };
    let mut base = vec![kv("operative", operative_text(evse.operative))];
    for c in &evse.connectors {
        base.push(kv(&format!("connector {}", c.id), c.status.as_str()));
        base.push(kv(
            "  EV",
            if c.ev_connected {
                "plugged in"
            } else {
                "not plugged in"
            },
        ));
        base.push(kv("  fault", if c.fault { "yes" } else { "no" }));
    }
    base.push(kv("meter", format!("{} Wh", evse.meter_wh)));
    match &evse.reservation {
        // On its own line: it needs the width of the pane.
        Some(r) => base.push(Line::from(Span::raw(format!(
            "reserved #{} for {} until {} UTC",
            r.id,
            sanitize(&r.id_token),
            when(r.expires_at)
        )))),
        None => base.push(kv("reservation", "none")),
    }
    if let Some(w) = evse.offered_w {
        let external = if evse.external_limit {
            " (external)"
        } else {
            ""
        };
        base.push(kv("limit", format!("{:.1} kW{external}", w / 1000.0)));
    }
    let mut tx = evse
        .transaction
        .as_ref()
        .map_or_else(|| vec![kv("transaction", "none")], transaction_lines);
    tx.extend(evse_extra_lines(evse));
    if transaction_first {
        let mut v = tx;
        v.extend(base);
        v
    } else {
        base.extend(tx);
        base
    }
}

fn scenario_lines(app: &App2) -> Vec<Line<'static>> {
    let sc = &app.scenario;
    if sc.name.is_empty() {
        return crate::ui::panes::idle_scenario_lines(
            app.selected_action()
                .map(|a| (a.label.as_str(), a.desc.as_str())),
        );
    }
    let state = match (&sc.report, sc.running) {
        (_, true) => "running".to_owned(),
        (Some(r), _) if r.passed => format!("passed in {} ms", r.duration_ms),
        (Some(r), _) if r.cancelled => "cancelled".to_owned(),
        (Some(_), _) => "FAILED".to_owned(),
        (None, false) => "not started".to_owned(),
    };
    let mut v = vec![Line::from(vec![
        Span::styled(
            sanitize(&sc.name),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {state}")),
    ])];
    for (i, (name, st)) in sc.steps.iter().enumerate() {
        let (mark, color, extra) = match st {
            StepState::Pending => (" ", crate::theme::SECONDARY, String::new()),
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
            StepState::Skipped => ("-", crate::theme::SECONDARY, " skipped".to_owned()),
        };
        v.push(Line::styled(
            format!("{mark} {}. {}{extra}", i + 1, sanitize(name)),
            Style::new().fg(color),
        ));
    }
    v
}

pub(super) fn actions(f: &mut Frame<'_>, app: &App2, area: Rect) {
    let idx = app.tab_actions();
    let room = usize::from(area.height.saturating_sub(2));
    let selected = app.action_sel[app.tab.index()];
    let start = window_start(selected, room, idx.len());
    let items: Vec<ListItem<'_>> = idx
        .iter()
        .skip(start)
        .take(room)
        .map(|&i| {
            let a = &app.catalog[i];
            let key = a.key.map_or_else(|| "  ".to_owned(), |k| format!("{k} "));
            let style = if app.reason_for2(a.id).is_some() {
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
    let mut state = ListState::default().with_selected(Some(selected.saturating_sub(start)));
    let list = List::new(items)
        .highlight_style(SELECTED)
        .block(with_markers(
            block(
                format!(
                    "Actions: {}{}",
                    app.tab.title(),
                    count_of(selected, room, idx.len())
                ),
                app.focus == Focus::Actions,
            ),
            start,
            idx.len().saturating_sub(start + room),
        ));
    f.render_stateful_widget(list, area, &mut state);
}

fn row_line(
    e: &LogEntry,
    inner_w: usize,
    selected: bool,
    time: TimeMode,
    mono: bool,
) -> Line<'static> {
    let mut spans = Vec::new();
    let stamp = time.prefix(e.ts);
    if !stamp.is_empty() {
        spans.push(Span::styled(stamp, dim()));
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
            let mark = if mono {
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

pub(super) fn log(f: &mut Frame<'_>, app: &App2, area: Rect) {
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
        .map(|(i, e)| {
            row_line(
                e,
                inner_w,
                Some(i) == sel && app.focus == Focus::Log,
                app.time,
                app.mono,
            )
        })
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
        Paragraph::new(lines).block(with_markers(
            block(truncate(&title, inner_w.max(1)), app.focus == Focus::Log),
            top,
            visible.len().saturating_sub(top + h),
        )),
        area,
    );
}
