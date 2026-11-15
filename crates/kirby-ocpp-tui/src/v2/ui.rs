//! Drawing of the OCPP 2.x UI. `draw2` is a pure function of the [`App2`]; it never mutates
//! it.

use kirby_ocpp_core::ocpp::v2::types::WireEnum;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::actions::Tab2;
use super::app::{App2, ConfirmAction2, Mode2};
use super::{browser, panes};
use crate::app::{Focus, NoticeLevel};
use crate::text::{sanitize, truncate, truncate_middle, width};
use crate::theme::{Level, level_mark};
use crate::ui::overlays::{self, PendingRow};
use crate::ui::{MIN_SIZE, WIDE, chip, dim, hint_line, hint_line_pinned, too_small};

/// Draws the whole UI: header, panes, footer and the overlay of the current mode.
pub fn draw2(f: &mut Frame<'_>, app: &App2) {
    draw_in(f, app, f.area());
    if app.mono {
        crate::theme::apply_monochrome(f.buffer_mut());
    }
}

fn draw_in(f: &mut Frame<'_>, app: &App2, area: Rect) {
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
    draw_overlay(f, app, area);
}

fn draw_body(f: &mut Frame<'_>, app: &App2, area: Rect) {
    if area.width >= WIDE {
        let [evses, mid, log] = Layout::horizontal([
            Constraint::Percentage(28),
            Constraint::Percentage(40),
            Constraint::Min(10),
        ])
        .areas(area);
        let [details, actions] =
            Layout::vertical([Constraint::Percentage(55), Constraint::Min(3)]).areas(mid);
        panes::evses(f, app, evses);
        panes::details(f, app, details);
        panes::actions(f, app, actions);
        panes::log(f, app, log);
    } else {
        let rows: usize = app
            .snapshot
            .evses
            .iter()
            .map(|e| e.connectors.len().max(1))
            .sum();
        let rows = u16::try_from(rows).unwrap_or(u16::MAX);
        let list_h = (rows + 3).min(7).min(area.height / 3).max(3);
        let mid_h = (area.height * 2 / 5).clamp(3, 14);
        let [evses, mid, log] = Layout::vertical([
            Constraint::Length(list_h),
            Constraint::Length(mid_h),
            Constraint::Min(3),
        ])
        .areas(area);
        panes::evses(f, app, evses);
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

fn draw_header(f: &mut Frame<'_>, app: &App2, area: Rect) {
    let s = &app.snapshot;
    let w = usize::from(area.width);
    let (conn_text, conn_color) = panes::connection_text(s.connection);
    let (reg_text, reg_color) = match s.registration {
        Some(r) if s.booted() => (r.as_str().to_owned(), Color::Green),
        Some(r) => (r.as_str().to_owned(), Color::Yellow),
        None => ("not booted".to_owned(), Color::DarkGray),
    };
    // The chips in the order they give way to a narrow terminal: the connection and the
    // registration always stay.
    let mut chips = vec![chip(&conn_text, conn_color), chip(&reg_text, reg_color)];
    let mut optional = vec![chip(&sanitize(&s.protocol), Color::Blue)];
    if !app.pending.is_empty() {
        optional.push(chip(
            &format!("{} pending", app.pending.len()),
            Color::Yellow,
        ));
    }
    let scenario = &app.scenario;
    if scenario.running {
        optional.push(chip("scenario running", Color::Yellow));
    } else if let Some(r) = &scenario.report {
        optional.push(if r.passed {
            chip("scenario passed", Color::Green)
        } else if r.cancelled {
            chip("scenario cancelled", Color::Yellow)
        } else {
            chip("scenario failed", Color::Red)
        });
    }
    for c in optional {
        let used: usize = chips.iter().map(|c| c.width() + 1).sum();
        if used + c.width() + 1 + 10 <= w {
            chips.push(c);
        }
    }
    let chips_w: usize = chips.iter().map(|c| c.width() + 1).sum();
    // A long id gives way (middle cut) so the connection state always stays visible.
    let id_room = w.saturating_sub(chips_w + 2).max(8);
    let mut spans = vec![Span::styled(
        format!(
            " {} ",
            truncate_middle(&sanitize(&s.charge_point_id), id_room)
        ),
        Style::new().add_modifier(Modifier::BOLD),
    )];
    for c in chips {
        spans.push(c);
        spans.push(Span::raw(" "));
    }
    let used: usize = spans.iter().map(Span::width).sum();
    if w > used + 4 {
        spans.push(Span::styled(
            format!("  {}", truncate(&sanitize(&app.url), w - used - 2)),
            dim(),
        ));
    }
    let evse_line = match app.selected_evse() {
        Some(e) => {
            let status = e.connectors.first().map_or("-", |c| c.status.as_str());
            let tx = e.transaction.as_ref().map_or_else(
                || "no transaction".to_owned(),
                |t| {
                    format!(
                        "tx {}",
                        sanitize(&t.transaction_id)
                            .chars()
                            .take(8)
                            .collect::<String>()
                    )
                },
            );
            format!(" EVSE {} | {status} | {tx} | {} Wh", e.id, e.meter_wh)
        }
        None => " no EVSE".to_owned(),
    };
    let lines = vec![
        Line::from(spans),
        Line::styled(truncate(&evse_line, w), dim()),
        tab_line(app, w),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn tab_line(app: &App2, w: usize) -> Line<'static> {
    let full: usize = Tab2::ALL.iter().map(|t| width(t.title()) + 5).sum();
    let active = Style::new()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let spans: Vec<Span<'static>> = Tab2::ALL
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

fn draw_footer(f: &mut Frame<'_>, app: &App2, area: Rect) {
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
    } else if let Some(label) = app.pending.first() {
        let more = match app.pending.len() {
            0 | 1 => String::new(),
            n => format!(" (+{})", n - 1),
        };
        let elapsed = crate::app::elapsed_text(app.now.saturating_duration_since(label.since));
        Line::styled(
            truncate(
                &format!(
                    " waiting... {} {elapsed}{more}  (W: list)",
                    sanitize(&label.label)
                ),
                w,
            ),
            Style::new().fg(Color::Yellow),
        )
    } else if let (Mode2::Normal, Some(a)) = (&app.mode, app.selected_action()) {
        match app.reason_for2(a.id) {
            Some(why) => Line::styled(
                truncate(&format!(" {}: disabled, {why}", a.label), w),
                Style::new().fg(Color::Yellow),
            ),
            None => Line::styled(truncate(&format!(" {}", a.desc), w), dim()),
        }
    } else {
        Line::raw("")
    };
    let line = if matches!(app.mode, Mode2::Normal) {
        hint_line_pinned(&hints(app), PINNED_HINTS, w)
    } else {
        hint_line(&hints(app), w)
    };
    f.render_widget(Paragraph::new(vec![first, line]), area);
}

/// Always on the footer in normal mode; the last hints to go in a narrow terminal.
const PINNED_HINTS: &[(&str, &str)] = &[("?", "help"), (":", "palette")];

/// `(key, label)` pairs, most important first.
fn hints(app: &App2) -> Vec<(&'static str, &'static str)> {
    match &app.mode {
        Mode2::Normal => {
            let mut v = Vec::new();
            if !app.pending.is_empty() {
                v.push(("W", "pending"));
            }
            if app.focus == Focus::Log {
                v.push(("Enter", "detail"));
                if app.tab == Tab2::Logs {
                    v.extend([
                        ("y", "copy"),
                        ("f", "filter"),
                        ("s", "search"),
                        ("p", "pause"),
                    ]);
                } else {
                    v.push(("5", "log keys"));
                }
            } else if app.focus == Focus::Actions {
                v.push(("Enter", "run"));
            }
            v.extend([
                ("↑↓", "move"),
                ("←→", "pane"),
                ("1-6", "tab"),
                ("[ ]", "EVSE"),
                ("q", "quit"),
            ]);
            v
        }
        Mode2::Pending(_) => vec![("↑↓", "select"), ("Esc", "close")],
        Mode2::Palette(_) => vec![("Enter", "run"), ("Esc", "close"), ("↑↓", "select")],
        Mode2::Form(_) => vec![
            ("Enter", "submit"),
            ("Esc", "cancel"),
            ("Tab", "next field"),
            ("←→", "choose/move"),
        ],
        Mode2::Choice(_) => vec![("Enter", "select"), ("Esc", "cancel"), ("↑↓", "move")],
        Mode2::Help => vec![("Esc", "close")],
        Mode2::Detail(_) => vec![("Esc", "close"), ("↑↓", "scroll"), ("c", "copy")],
        Mode2::Confirm(c) if c.action == ConfirmAction2::Quit => {
            vec![("y/q", "quit"), ("n/Esc", "stay")]
        }
        Mode2::Confirm(_) => vec![("y", "yes"), ("n", "no")],
        Mode2::DeviceModel(b) if b.filtering => {
            vec![("Enter", "keep"), ("Esc", "clear"), ("↑↓", "move")]
        }
        Mode2::DeviceModel(_) => vec![
            ("Enter", "edit/show"),
            ("/", "filter"),
            ("↑↓", "move"),
            ("Esc", "close"),
        ],
    }
}

fn draw_overlay(f: &mut Frame<'_>, app: &App2, area: Rect) {
    match &app.mode {
        Mode2::Normal => {}
        Mode2::Palette(p) => overlays::palette(f, p, area, |i| {
            app.catalog.get(i).map_or_else(
                || (String::new(), true),
                |a| {
                    (
                        format!("{}: {}", a.tab.title(), a.label),
                        app.reason_for2(a.id).is_some(),
                    )
                },
            )
        }),
        Mode2::Form(form) => overlays::form(f, form, area),
        Mode2::Choice(c) => overlays::choice(f, c, area),
        Mode2::Help => overlays::help(f, &help_lines2(), area),
        Mode2::Detail(d) => overlays::detail(f, d, area),
        Mode2::Confirm(c) => {
            overlays::confirm(f, &c.message, c.action == ConfirmAction2::Quit, area);
        }
        Mode2::DeviceModel(b) => browser::draw(f, &app.snapshot.variables, b, area),
        Mode2::Pending(sel) => {
            let rows: Vec<PendingRow> = app
                .pending
                .iter()
                .map(|p| PendingRow {
                    elapsed: crate::app::elapsed_text(app.now.saturating_duration_since(p.since)),
                    text: sanitize(&p.label),
                    cancellable: false,
                })
                .collect();
            overlays::pending_list(f, area, &rows, *sel);
        }
    }
}

const HELP2: &[&str] = &[
    "First steps: the station connects and boots at startup (the header shows the state; on the",
    "     Station tab x disconnects and c connects again). Press : and type a name",
    "     (BootNotification, Heartbeat, Present token) to run it; tab 6 runs scenarios.",
    "     Everything lands in the log.",
    "",
    "1-6 / Tab / Shift-Tab   switch tab",
    "Up Down j k             move in the focused pane",
    "Left Right              change focused pane (EVSEs, actions, log)",
    "[ ]                     previous / next EVSE",
    "Enter                   run action / open log row",
    "letters                 accelerators of the CURRENT tab only (shown in the list)",
    ":  or Ctrl-P            command palette (every action, fuzzy search)",
    "/                       search where there is a search (log pane of the Logs tab, device",
    "                        model), otherwise the command palette",
    "W                       commands waiting for the CSMS, with their elapsed time",
    "Esc                     close dialog / clear log search",
    "?                       this help",
    "q or Ctrl-C             quit (asks while a scenario or transaction runs)",
    "",
    "Station tab: b BootNotification, h Heartbeat, r reconnect, x disconnect, c connect,",
    "     K device model (station-level variables: / filters, Enter edits a writable one),",
    "     S security event (type and optional techInfo)",
    "EVSE tab: p plug / unplug, f fault / clear fault, n StatusNotification. The connector",
    "     status is derived from the EVSE state and cannot be set by hand.",
    "Transaction tab: a Authorize, t present token (what it starts depends on TxStartPoint),",
    "     e stop transaction with a reason, m meter +120 Wh, c meter custom, g charging state",
    "Data tab: d DataTransfer, v vendor ping, r raw CALL (checked against the schema)",
    "",
    "Log pane: Enter opens the detail of the selected row on every tab. On the Logs tab (5):",
    "     y copy row, Y copy visible, f filter, s search, p pause/follow, G newest,",
    "     e export JSONL, c clear (asks first)",
    "Scenarios tab (6): o w s i run the first scenarios, l load a file, x cancel",
    "",
    "Not available for OCPP 2.0.1: setting a connector status, rules, configuration keys (the",
    "     device model replaces them), local authorization list, charging profiles,",
    "     certificates, diagnostics, firmware, and cancelling a waiting command.",
    "",
    "Without color (--color never or NO_COLOR): chips and selections are reverse video, errors",
    "     bold, and log lines start with a marker: i info, + success, ! warning, x error.",
];

/// The lines of the help popup.
pub fn help_lines2() -> Vec<&'static str> {
    HELP2.to_vec()
}
