//! Drawing. `draw` is a pure function of the [`App`]; it never mutates it.

mod browser;
pub(crate) mod overlays;
pub(crate) mod panes;

pub use overlays::{
    PendingRow, content_height, help_sections, pending_line, pending_list, visible_scroll,
};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::actions::Tab;
use crate::app::{App, ConfirmAction, Mode, NoticeLevel};
use crate::fleet::{FleetApp, station_marks, visible_range};
use crate::text::{sanitize, truncate, width};
use crate::theme::{Level, level_mark};

/// Width from which the three-column layout is used.
pub const WIDE: u16 = 110;
pub const MIN_SIZE: (u16, u16) = (24, 8);

/// Secondary text style (labels, hints, inactive items): readable, not dimmed.
pub fn dim() -> Style {
    crate::theme::secondary()
}

pub fn draw(f: &mut Frame<'_>, app: &App) {
    draw_in(f, app, f.area());
    if app.mono {
        crate::theme::apply_monochrome(f.buffer_mut());
    }
}

/// Draws the whole fleet UI: with several stations a selector row on top and the
/// selected station below; with one station exactly [`draw`].
pub fn draw_fleet(f: &mut Frame<'_>, fleet: &FleetApp) {
    draw_fleet_colored(f, fleet);
    if fleet.current().mono {
        crate::theme::apply_monochrome(f.buffer_mut());
    }
}

fn draw_fleet_colored(f: &mut Frame<'_>, fleet: &FleetApp) {
    if !fleet.is_fleet() || f.area().height < 2 {
        draw_in(f, fleet.current(), f.area());
        return;
    }
    let Some([strip, rest]) = split_selector(f) else {
        return;
    };
    let stations = fleet.apps.iter().map(|app| {
        let (glyph, tx) = station_marks(&app.snapshot);
        (app.snapshot.charge_point_id.as_str(), glyph, tx)
    });
    draw_selector(f, stations, fleet.selected, strip);
    draw_in(f, fleet.current(), rest);
}

/// Splits the screen of a fleet into the selector row and the station below it. Draws the
/// "too small" notice and returns `None` when the terminal cannot hold both.
pub(crate) fn split_selector(f: &mut Frame<'_>) -> Option<[Rect; 2]> {
    // The selector row takes one line: a fleet needs one more row than a single station.
    let need = (MIN_SIZE.0, MIN_SIZE.1 + 1);
    if f.area().width < need.0 || f.area().height < need.1 {
        too_small(f, f.area(), need);
        return None;
    }
    Some(Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(f.area()))
}

/// How the selector draws a connection glyph of the fleet model: the shape tells the state
/// without the color.
fn selector_glyph(glyph: char) -> (char, Color, &'static str) {
    match glyph {
        '+' => ('●', Color::Green, "connected"),
        '~' => ('◐', Color::Yellow, "connecting"),
        _ => ('○', Color::Red, "offline"),
    }
}

/// `id<glyph>[*]` per station (id, glyph, transaction running), windowed around the selected
/// one, then the position and a legend of the markers in use. The ids are sanitized here.
pub(crate) fn draw_selector<'a>(
    f: &mut Frame<'_>,
    stations: impl ExactSizeIterator<Item = (&'a str, char, bool)>,
    selected: usize,
    area: Rect,
) {
    let stations: Vec<(&str, char, bool)> = stations.collect();
    let total = stations.len();
    let count = format!(" {}/{total}", selected + 1);
    let mut legend: Vec<String> = Vec::new();
    for g in ['+', '~', '-'] {
        let (shape, _, name) = selector_glyph(g);
        if stations.iter().any(|s| selector_glyph(s.1).0 == shape) {
            legend.push(format!("{shape} {name}"));
        }
    }
    if stations.iter().any(|s| s.2) {
        legend.push("* transaction".to_owned());
    }
    let mut legend = format!("  {}", legend.join("  "));
    let total_w = usize::from(area.width);
    // The legend gives way when it would squeeze the stations.
    if total_w < width(&legend) + width(&count) + 28 {
        legend.clear();
    }
    let avail = total_w.saturating_sub(width(&count) + width(&legend) + 4);
    let labels: Vec<(String, Color)> = stations
        .iter()
        .map(|&(id, glyph, tx)| {
            let (shape, color, _) = selector_glyph(glyph);
            let tx = if tx { "*" } else { "" };
            let id = crate::text::sanitize(id);
            (format!(" {id} {shape}{tx} "), color)
        })
        .collect();
    let widths: Vec<usize> = labels.iter().map(|(l, _)| width(l)).collect();
    let (start, end) = visible_range(&widths, selected, avail);
    let mut spans = vec![Span::styled(if start > 0 { "‹ " } else { "  " }, dim())];
    for (i, (label, color)) in labels.iter().enumerate().take(end).skip(start) {
        let style = if i == selected {
            Style::new().fg(Color::Black).bg(*color)
        } else {
            Style::new().fg(*color)
        };
        spans.push(Span::styled(truncate(label, avail.max(1)), style));
        spans.push(Span::raw(" "));
    }
    if end < total {
        spans.push(Span::styled("› ", dim()));
    }
    spans.push(Span::styled(count, dim()));
    spans.push(Span::styled(legend, dim()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The marker for rows that are cut off above a pane.
#[must_use]
pub fn more_above(n: usize) -> String {
    format!("▲ {n} more")
}

/// The marker for rows that are cut off below a pane.
#[must_use]
pub fn more_below(n: usize) -> String {
    format!("▼ {n} more")
}

/// Puts the cut-off markers on the border of a pane: `▲ N more` top right, `▼ N more` bottom
/// right. Nothing is added for a count of zero.
pub(crate) fn with_markers(block: Block<'static>, above: usize, below: usize) -> Block<'static> {
    let mark = |n: usize, text: String| (n > 0).then(|| Line::styled(text, dim()).right_aligned());
    let mut block = block;
    if let Some(line) = mark(above, more_above(above)) {
        block = block.title_top(line);
    }
    if let Some(line) = mark(below, more_below(below)) {
        block = block.title_bottom(line);
    }
    block
}

/// The first row of a window of `rows` rows over `len` rows that keeps `selected` in view,
/// roughly in the middle.
#[must_use]
pub fn window_start(selected: usize, rows: usize, len: usize) -> usize {
    if rows == 0 || len <= rows {
        return 0;
    }
    selected.saturating_sub(rows / 2).min(len - rows)
}

/// ` n/N` for a pane title when a list of `len` rows is cut to `rows`; empty otherwise.
#[must_use]
pub fn count_of(selected: usize, rows: usize, len: usize) -> String {
    if len > rows {
        format!(" {}/{len}", selected + 1)
    } else {
        String::new()
    }
}

/// Wraps styled lines to `w` cells by breaking them at the cell edge, keeping the styles.
/// A pane that scrolls needs the number of rows its text takes, which a word-wrapping
/// widget does not tell.
#[must_use]
pub fn wrap_lines(lines: Vec<Line<'static>>, w: usize) -> Vec<Line<'static>> {
    use unicode_segmentation::UnicodeSegmentation;
    let w = w.max(1);
    let mut out = Vec::new();
    for line in lines {
        let mut row: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        for span in line.spans {
            let mut piece = String::new();
            for g in span.content.graphemes(true) {
                let gw = width(g);
                if used + gw > w && used > 0 {
                    row.push(Span::styled(std::mem::take(&mut piece), span.style));
                    out.push(Line::from(std::mem::take(&mut row)).style(line.style));
                    used = 0;
                }
                piece.push_str(g);
                used += gw;
            }
            if !piece.is_empty() {
                row.push(Span::styled(piece, span.style));
            }
        }
        out.push(Line::from(row).style(line.style));
    }
    out
}

/// Draws `lines` (already wrapped) in a pane from row `scroll`, with the cut-off markers on
/// the border. `max_scroll` is set to the largest useful `scroll`.
pub(crate) fn scrolled_text(
    f: &mut Frame<'_>,
    area: Rect,
    block: Block<'static>,
    lines: Vec<Line<'static>>,
    scroll: usize,
    max_scroll: &std::cell::Cell<usize>,
) {
    let rows = usize::from(area.height.saturating_sub(2));
    let max = lines.len().saturating_sub(rows);
    max_scroll.set(max);
    let start = scroll.min(max);
    let shown: Vec<Line<'static>> = lines.into_iter().skip(start).take(rows).collect();
    let below = max - start;
    f.render_widget(
        Paragraph::new(shown).block(with_markers(block, start, below)),
        area,
    );
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

/// Rows of the left-hand list at a narrow width: its rows and header, borders included, but
/// never more than `cap`; the actions and details panes get the rest.
pub(crate) fn list_height(rows: usize, area_height: u16, cap: u16) -> u16 {
    let want = u16::try_from(rows + 3).unwrap_or(u16::MAX);
    // A short body keeps the header and about two rows.
    let cap = if area_height < 24 { cap.min(5) } else { cap };
    want.min(cap).min(area_height / 3).max(3)
}

/// Rows of the actions and details band below the list at a narrow width: a little more than
/// half of what the list leaves.
pub(crate) fn band_height(area_height: u16, list: u16) -> u16 {
    (area_height.saturating_sub(list) * 11 / 20).clamp(3, 16)
}

fn draw_body(f: &mut Frame<'_>, app: &App, area: Rect) {
    if app.zoom {
        match app.focus {
            crate::app::Focus::Connectors => panes::connectors(f, app, area),
            crate::app::Focus::Details => panes::details(f, app, area),
            crate::app::Focus::Actions => panes::actions(f, app, area),
            crate::app::Focus::Log => panes::log(f, app, area),
        }
        return;
    }
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
        let conn_h = list_height(usize::from(rows), area.height, 7);
        let mid_h = band_height(area.height, conn_h);
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
        } else if app.focus == crate::app::Focus::Details {
            // No room for both: the pane that has the keys takes the place.
            panes::details(f, app, mid);
        } else {
            panes::actions(f, app, mid);
        }
        panes::log(f, app, log);
    }
}

pub(crate) fn chip(text: &str, color: Color) -> Span<'static> {
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
        None => ("not booted".to_owned(), crate::theme::SECONDARY),
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
    // The protocol chip goes after the registration when the line has room for it.
    let protocol = chip("OCPP 1.6", Color::Blue);
    let used: usize = spans.iter().map(Span::width).sum();
    if w >= used + protocol.width() + 1 + 10 {
        spans.push(Span::raw(" "));
        spans.push(protocol);
    }
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
                    " waiting... {} {elapsed}{more}  (W: list, cancel)",
                    sanitize(&label.label)
                ),
                w,
            ),
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
            if !app.pending.is_empty() {
                v.push(("W", "pending"));
            }
            if app.focus == Focus::Log {
                v.push(("Enter", "detail"));
                // The log letters are accelerators of the Logs tab only.
                if app.tab == Tab::Logs {
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
                // Enter on the connector list does nothing, so it is not hinted there.
                v.push(("Enter", "run"));
            }
            if app.zoom {
                v.insert(0, ("z/Esc", "back"));
            }
            v.extend([("↑↓", "move"), ("←→", "pane"), ("1-7", "tab")]);
            if app.in_fleet {
                // `[ ]` goes last: at 80 columns the line has room for one hint less.
                v.extend([("< >", "station"), ("q", "quit"), ("[ ]", "connector")]);
            } else {
                v.extend([("[ ]", "connector"), ("q", "quit")]);
            }
            if !app.zoom {
                v.push(("z", "zoom"));
            }
            v
        }
        Mode::Pending(_) => vec![("x", "cancel"), ("↑↓", "select"), ("Esc", "close")],
        Mode::Palette(_) => vec![("Enter", "run"), ("Esc", "close"), ("↑↓", "select")],
        Mode::Form(_) => vec![
            ("Enter", "submit"),
            ("Esc", "cancel"),
            ("Tab", "next field"),
            ("←→", "choose/move"),
        ],
        Mode::Choice(_) => vec![("Enter", "select"), ("Esc", "cancel"), ("↑↓", "move")],
        Mode::Help(_) => vec![("←→", "section"), ("↑↓", "scroll"), ("Esc", "close")],
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
