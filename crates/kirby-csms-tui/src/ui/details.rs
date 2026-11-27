//! The details pane: a summary line, the connection line every protocol shares, the counters
//! of the protocol, further lines, and the connector and transaction tables. Whatever is cut
//! says so with `▲` / `▼ N more`, and the selected transaction stays in view.

use std::fmt::Write as _;

use kirby_csms_core::{StationInfo, StationState, StationState2, TransactionStatus};
use kirby_ocpp_core::ocpp::v2::types::WireEnum;
use kirby_ocpp_tui::text::{sanitize, truncate};
use kirby_ocpp_tui::ui::dim;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Row, Table, Wrap};

use super::layout::{self, Content, window_start};
use super::panes::{SELECTED, age, block, clock, empty_state, markers, status_color};
use crate::app::{App, Focus};

/// Width from which the connector and transaction tables sit side by side.
const SIDE_BY_SIDE: u16 = 140;

/// The text of a pane's content, built once per draw.
struct Lines {
    head: Vec<String>,
    extras: Vec<String>,
}

impl Lines {
    fn of(app: &App) -> Self {
        let info = app.info.as_ref();
        let state = app.state.as_ref();
        let mut head = Vec::new();
        if let Some(s) = state {
            head.push(summary(s));
        }
        head.extend(first_line(app, info, state));
        if let Some(s) = state {
            head.push(counters(info, s));
        }
        let extras = state.map_or_else(Vec::new, |s| extras(app, info, s));
        Self { head, extras }
    }
}

fn content(lines: &Lines, state: Option<&StationState>) -> Content {
    let (connectors, transactions) = state.map_or((0, 0), |s| match &s.v2 {
        Some(_) => (s.connector_statuses().len(), tx_count(s)),
        None => (s.connectors.len(), s.transactions.len()),
    });
    Content {
        head: lines.head.len(),
        extras: lines.extras.len(),
        connectors,
        transactions,
    }
}

fn tx_count(s: &StationState) -> usize {
    s.v2.as_ref()
        .map_or(s.transactions.len(), |v2| v2.transactions.len())
}

/// Rows of the transaction table that hold data in `area` (the details pane), so a key can
/// tell whether the selected row is on screen.
pub fn transaction_rows(app: &App, area: Rect) -> usize {
    let inner = Rect {
        x: 0,
        y: 0,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let lines = Lines::of(app);
    let content = content(&lines, app.state.as_ref());
    let split = layout::split(inner.height, content, inner.width >= SIDE_BY_SIDE);
    layout::rows_shown(split.transactions, content.transactions)
}

pub fn details(f: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(id) = app.selected.as_deref() else {
        let lines = if app.stations.is_empty() {
            empty_state(app)
        } else {
            vec![Line::styled("no station selected", dim())]
        };
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(block("Station".to_owned(), None, false)),
            area,
        );
        return;
    };
    let inner_w = usize::from(area.width.saturating_sub(2));
    let lines = Lines::of(app);
    let state = app.state.as_ref();
    let content = content(&lines, state);
    let wide = area.width.saturating_sub(2) >= SIDE_BY_SIDE;
    let split = layout::split(area.height.saturating_sub(2), content, wide);
    let cut =
        (content.head - usize::from(split.head)) + (content.extras - usize::from(split.extras));
    let outer = block(
        truncate(&format!("Station {}", sanitize(id)), inner_w.max(1)),
        // Cut text lines say so on the border; a cut table has its own marker row.
        (cut > 0).then(|| format!(" \u{25bc} {cut} more lines ")),
        app.focus == Focus::Details,
    );
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let [head, extras, tables] = Layout::vertical([
        Constraint::Length(split.head),
        Constraint::Length(split.extras),
        Constraint::Min(0),
    ])
    .areas(inner);
    let text = |lines: &[String], rows: u16| -> Vec<Line<'static>> {
        lines
            .iter()
            .take(usize::from(rows))
            .map(|l| Line::raw(truncate(&sanitize(l), inner_w)))
            .collect()
    };
    f.render_widget(Paragraph::new(text(&lines.head, split.head)), head);
    f.render_widget(Paragraph::new(text(&lines.extras, split.extras)), extras);
    let Some(state) = state else {
        return;
    };
    let (conn_area, tx_area) = if wide {
        let [c, t] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .areas(tables);
        (c, t)
    } else {
        let [c, t] = Layout::vertical([
            Constraint::Length(split.connectors),
            Constraint::Length(split.transactions),
        ])
        .areas(tables);
        (c, t)
    };
    let conn_area = Rect {
        height: conn_area.height.min(split.connectors),
        ..conn_area
    };
    let tx_area = Rect {
        height: tx_area.height.min(split.transactions),
        ..tx_area
    };
    if let Some(v2) = &state.v2 {
        connectors_v2(f, app, state, v2, conn_area);
        transactions_v2(f, app, v2, tx_area);
    } else {
        connectors(f, app, state, conn_area);
        transactions(f, app, state, tx_area);
    }
}

// ---- text ------------------------------------------------------------------------------

fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// `1 EVSE · 1 tx · 7.2 kW · 2 security events`: what the station holds at a glance.
fn summary(s: &StationState) -> String {
    let mut parts = Vec::new();
    if let Some(v2) = &s.v2 {
        parts.push(plural(v2.evses.len(), "EVSE"));
    } else {
        let n = s.connectors.keys().filter(|id| **id >= 1).count();
        parts.push(plural(n, "connector"));
    }
    parts.push(format!("{} tx", s.active_transaction_count()));
    if let Some(watts) = active_power(s) {
        parts.push(format!("{:.1} kW", watts / 1000.0));
    }
    if s.security_events_total > 0 {
        let n = usize::try_from(s.security_events_total).unwrap_or(usize::MAX);
        parts.push(plural(n, "security event"));
    }
    parts.join(" \u{b7} ")
}

/// The power of the running transactions together, when any of them reported one.
fn active_power(s: &StationState) -> Option<f64> {
    let legacy = s
        .transactions
        .iter()
        .filter(|t| t.status == TransactionStatus::Active)
        .filter_map(|t| t.power_w);
    let v2 =
        s.v2.iter()
            .flat_map(|v2| &v2.transactions)
            .filter(|t| t.status == TransactionStatus::Active)
            .filter_map(|t| t.power_w);
    let readings: Vec<f64> = legacy.chain(v2).collect();
    (!readings.is_empty()).then(|| readings.iter().sum())
}

/// The line every protocol starts with: the connection, the protocol, the boot and the last
/// heartbeat.
fn first_line(
    app: &App,
    info: Option<&StationInfo>,
    state: Option<&StationState>,
) -> Option<String> {
    let now = app.now;
    let mut parts = Vec::new();
    if let Some(i) = info {
        parts.push(if i.connected {
            let since = i
                .connected_at
                .map_or_else(String::new, |t| format!(" for {}", age(now, t)));
            format!("connected{since}")
        } else {
            let since = i
                .disconnected_at
                .map_or_else(String::new, |t| format!(" {} ago", age(now, t)));
            format!("disconnected{since}")
        });
        parts.push(match i.subprotocol.as_deref() {
            Some(p) => format!("OCPP {}", p.strip_prefix("ocpp").unwrap_or(p)),
            None => "no subprotocol".to_owned(),
        });
    }
    if let Some(s) = state {
        parts.push(match &s.boot {
            Some(b) => truncate(&format!("booted {} {}", b.vendor, b.model), 32),
            None => "not booted".to_owned(),
        });
        parts.push(match s.last_heartbeat_at {
            Some(t) => format!("heartbeat {} ago", age(now, t)),
            None => "no heartbeat".to_owned(),
        });
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// The counters of the station's protocol.
fn counters(info: Option<&StationInfo>, s: &StationState) -> String {
    match &s.v2 {
        Some(v2) => {
            let c = &v2.counters;
            format!(
                "events {}  duplicates {}  rejected {}  meters {}  reports {}",
                c.transaction_events,
                c.duplicate_events,
                c.rejected_events,
                c.meter_values,
                c.notify_reports
            )
        }
        None => format!(
            "calls received {}  failed {}  unknown stops {}",
            info.map_or(0, |i| i.received_calls),
            s.failed_calls,
            s.unknown_stops
        ),
    }
}

/// The lines under the counters, most useful first: the boot details, firmware and diagnostics,
/// the progress of a report, the security information.
fn extras(app: &App, info: Option<&StationInfo>, s: &StationState) -> Vec<String> {
    let now = app.now;
    let mut out = Vec::new();
    let mut identity = Vec::new();
    if let Some(reason) = s.v2.as_ref().and_then(|v2| v2.boot_reason) {
        identity.push(format!("boot reason {}", reason.as_str()));
    }
    if let Some(b) = &s.boot {
        if let Some(fw) = &b.firmware_version {
            identity.push(format!("fw {fw}"));
        }
        if let Some(sn) = b
            .charge_point_serial_number
            .as_ref()
            .or(b.charge_box_serial_number.as_ref())
        {
            identity.push(format!("sn {sn}"));
        }
        if let Some(r) = b.registration {
            let mut answered = format!("answered {r}");
            if let Some(iv) = b.interval {
                let _ = write!(answered, " every {iv}s");
            }
            identity.push(answered);
        }
    }
    if let Some(addr) = info.and_then(|i| i.remote_addr) {
        identity.push(format!("remote {addr}"));
    }
    if !identity.is_empty() {
        out.push(identity.join("  "));
    }
    let mut statuses = Vec::new();
    if let Some(fw) = &s.firmware_status {
        statuses.push(format!("firmware {} ({} ago)", fw.status, age(now, fw.at)));
    }
    if let Some(d) = &s.diagnostics_status {
        statuses.push(format!("diagnostics {} ({} ago)", d.status, age(now, d.at)));
    }
    if !statuses.is_empty() {
        out.push(statuses.join("  "));
    }
    if let Some(r) = s.v2.as_ref().and_then(|v2| v2.last_report.as_ref()) {
        out.push(format!(
            "report #{} seqNo {} tbc {}",
            r.request_id, r.seq_no, r.tbc
        ));
    }
    out.extend(security_lines(app, s));
    out
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
    for (kind, c) in &s.installed_certificates {
        let count = if c.total > c.certificates.len() {
            format!("{} of {}", c.certificates.len(), c.total)
        } else {
            c.total.to_string()
        };
        parts.push(format!(
            "installed {kind}: {count} ({}, {} ago)",
            c.status,
            age(app.now, c.at)
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

// ---- tables ----------------------------------------------------------------------------

/// Draws `rows` (the window of a table of `total` rows that starts at `start`) under `header`,
/// with a `▲` / `▼ N more` row at the bottom when rows are cut.
fn table<'a, const N: usize>(
    f: &mut Frame<'_>,
    area: Rect,
    header: [&'a str; N],
    widths: [Constraint; N],
    rows: Vec<Row<'a>>,
    (start, total): (usize, usize),
) {
    if area.height == 0 {
        return;
    }
    let below = total.saturating_sub(start + rows.len());
    let cut = markers(start, below);
    let (grid, marker) = match &cut {
        Some(_) if area.height >= 2 => {
            let grid = Rect {
                height: area.height - 1,
                ..area
            };
            let marker = Rect {
                y: area.y + area.height - 1,
                height: 1,
                ..area
            };
            (grid, Some(marker))
        }
        _ => (area, None),
    };
    f.render_widget(
        Table::new(rows, widths).header(Row::new(header).style(dim())),
        grid,
    );
    if let (Some(text), Some(marker)) = (cut, marker) {
        f.render_widget(
            Paragraph::new(Line::styled(text.trim().to_owned(), dim())),
            marker,
        );
    }
}

fn connectors(f: &mut Frame<'_>, app: &App, state: &StationState, area: Rect) {
    let total = state.connectors.len();
    let shown = layout::rows_shown(area.height, total);
    let rows = state
        .connectors
        .iter()
        .take(shown)
        .map(|(id, c)| {
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
        })
        .collect();
    table(
        f,
        area,
        ["#", "Status", "Error", "Info", "Upd"],
        [
            Constraint::Length(4),
            Constraint::Length(13),
            Constraint::Length(12),
            Constraint::Min(4),
            Constraint::Length(5),
        ],
        rows,
        (0, total),
    );
}

fn connectors_v2(
    f: &mut Frame<'_>,
    app: &App,
    state: &StationState,
    v2: &StationState2,
    area: Rect,
) {
    let statuses = state.connector_statuses();
    let shown = layout::rows_shown(area.height, statuses.len());
    let rows = statuses
        .iter()
        .take(shown)
        .map(|(id, status)| {
            let updated = v2
                .evses
                .iter()
                .flat_map(|(e, ev)| {
                    ev.connectors
                        .iter()
                        .map(move |(c, s)| (format!("{e}/{c}"), s))
                })
                .find(|(key, _)| key == id)
                .map_or_else(|| "-".to_owned(), |(_, s)| age(app.now, s.received_at));
            Row::new(vec![
                truncate(&sanitize(id), 10),
                truncate(&sanitize(status), 12),
                updated,
            ])
            .style(Style::new().fg(status_color(status)))
        })
        .collect();
    table(
        f,
        area,
        ["EVSE/conn", "Status", "Upd"],
        [
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(5),
        ],
        rows,
        (0, statuses.len()),
    );
}

/// Row style of transaction `index` (newest first): the selection, else by state.
fn tx_style(app: &App, index: usize, selected: usize, active: bool) -> Style {
    if app.focus == Focus::Details && index == selected {
        SELECTED
    } else if active {
        Style::new().fg(Color::Cyan)
    } else {
        dim()
    }
}

fn transactions(f: &mut Frame<'_>, app: &App, state: &StationState, area: Rect) {
    let total = state.transactions.len();
    let shown = layout::rows_shown(area.height, total);
    let selected = app.transaction_selection.min(total.saturating_sub(1));
    let start = window_start(selected, total, shown);
    // Newest first: that is what one looks for.
    let rows = state
        .transactions
        .iter()
        .rev()
        .enumerate()
        .skip(start)
        .take(shown)
        .map(|(index, t)| {
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
            .style(tx_style(app, index, selected, active))
        })
        .collect();
    table(
        f,
        area,
        ["Tx", "C", "idTag", "Meter", "Wh", "W", "State", "Time"],
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
        rows,
        (start, total),
    );
}

fn transactions_v2(f: &mut Frame<'_>, app: &App, v2: &StationState2, area: Rect) {
    // The wire names of the charging state fit from this width; below it they would clip
    // (two of them to the same text), so the short forms are used.
    let short_states = area.width < 90;
    let total = v2.transactions.len();
    let shown = layout::rows_shown(area.height, total);
    let selected = app.transaction_selection.min(total.saturating_sub(1));
    let start = window_start(selected, total, shown);
    let rows = v2
        .transactions
        .iter()
        .rev()
        .enumerate()
        .skip(start)
        .take(shown)
        .map(|(index, tx)| {
            let loc = format!(
                "{}/{}",
                tx.evse.map_or_else(|| "-".to_owned(), |v| v.to_string()),
                tx.connector
                    .map_or_else(|| "-".to_owned(), |v| v.to_string())
            );
            let active = tx.status == TransactionStatus::Active;
            let state = if active {
                "active".to_owned()
            } else {
                format!("ended {}", tx.stopped_reason.map_or("-", |r| r.as_str()))
            };
            Row::new(vec![
                truncate(&sanitize(&tx.id), 8),
                loc,
                truncate(&sanitize(tx.id_token.as_deref().unwrap_or("-")), 10),
                tx.charging_state
                    .map_or("-", |s| charging_state_text(s.as_str(), short_states))
                    .to_owned(),
                tx.energy_wh
                    .map_or_else(|| "-".to_owned(), |v| format!("{v:.0}")),
                tx.power_w
                    .map_or_else(|| "-".to_owned(), |v| format!("{v:.0}")),
                truncate(&sanitize(&state), 24),
                clock(tx.started_at),
            ])
            .style(tx_style(app, index, selected, active))
        })
        .collect();
    table(
        f,
        area,
        [
            "Tx", "EVSE/C", "idToken", "Charging", "Wh", "W", "State", "Time",
        ],
        [
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(if short_states { 8 } else { 13 }),
            Constraint::Length(7),
            Constraint::Length(6),
            Constraint::Length(24),
            Constraint::Min(8),
        ],
        rows,
        (start, total),
    );
}

/// A charging state as the transaction table shows it: the wire name, or in a narrow table a
/// short form (`EVConn`, `SuspEV`, `SuspEVSE`) that fits the 8-column state cell.
fn charging_state_text(wire: &str, short: bool) -> &str {
    if !short {
        return wire;
    }
    match wire {
        "EVConnected" => "EVConn",
        "SuspendedEV" => "SuspEV",
        "SuspendedEVSE" => "SuspEVSE",
        other => other,
    }
}
