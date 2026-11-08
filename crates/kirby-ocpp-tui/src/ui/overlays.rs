use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use super::{dim, popup_width};
use crate::app::{App, Confirm, ConfirmAction, Detail, Mode};
use crate::forms::{Choice, Field, FieldKind, Form, SECRET_HINT};
use crate::palette::Palette;
use crate::text::{sanitize, truncate, width, wrap_help, wrap_text};

pub(super) const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

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

pub(super) fn popup(f: &mut Frame<'_>, rect: Rect, title: &str) -> Rect {
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Cyan))
        .title(truncate(title, usize::from(rect.width.saturating_sub(4))));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect) {
    match &app.mode {
        Mode::Normal => {}
        Mode::Palette(p) => palette(f, app, p, area),
        Mode::Form(form) => self::form(f, form, area),
        Mode::Choice(c) => choice(f, c, area),
        Mode::Help => help(f, app.in_fleet, area),
        Mode::Detail(d) => detail(f, d, area),
        Mode::Confirm(c) => confirm(f, c, area),
        Mode::Browser(b) => super::browser::draw(f, app, b, area),
        Mode::Pending(sel) => {
            let rows: Vec<PendingRow> = app
                .pending
                .iter()
                .map(|p| PendingRow {
                    elapsed: crate::app::elapsed_text(app.now.saturating_duration_since(p.since)),
                    text: sanitize(&p.label),
                    cancellable: p.action.is_some(),
                })
                .collect();
            pending_list(f, area, &rows, *sel);
        }
    }
}

/// One row of the pending-commands popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRow {
    /// Time since the command was sent, as [`crate::app::elapsed_text`] writes it.
    pub elapsed: String,
    /// What the command is (already sanitized).
    pub text: String,
    /// `x` can cancel it.
    pub cancellable: bool,
}

/// The line of one pending row.
#[must_use]
pub fn pending_line(row: &PendingRow) -> String {
    let note = if row.cancellable { "" } else { "  (no cancel)" };
    format!("{:>7}  {}{note}", row.elapsed, row.text)
}

/// The popup that lists commands waiting for an answer; `x` cancels the selected row.
pub fn pending_list(f: &mut Frame<'_>, area: Rect, rows: &[PendingRow], selected: usize) {
    let h = u16::try_from(rows.len().max(1) + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2))
        .max(3);
    let rect = centered(area, 72.min(area.width.saturating_sub(2)), h);
    let inner = popup(f, rect, &format!("Pending calls ({})", rows.len()));
    let w = usize::from(inner.width);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled("nothing is waiting", dim())),
            inner,
        );
        return;
    }
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|r| ListItem::new(truncate(&pending_line(r), w)))
        .collect();
    let mut state = ListState::default().with_selected(Some(selected.min(rows.len() - 1)));
    f.render_stateful_widget(
        List::new(items).highlight_style(SELECTED),
        inner,
        &mut state,
    );
}

fn palette(f: &mut Frame<'_>, app: &App, p: &Palette, area: Rect) {
    let h = u16::try_from(p.results.len() + 3)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2))
        .max(4);
    let rect = centered(area, 72.min(area.width.saturating_sub(2)), h);
    let inner = popup(f, rect, "Command palette");
    if inner.height < 2 {
        return;
    }
    let w = usize::from(inner.width);
    let (shown, col) = p.query.window(w.saturating_sub(3));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/ ", Style::new().fg(Color::Cyan)),
            Span::raw(shown),
        ])),
        Rect { height: 1, ..inner },
    );
    f.set_cursor_position(Position::new(
        inner.x + 2 + u16::try_from(col).unwrap_or(0),
        inner.y,
    ));
    let items: Vec<ListItem<'_>> = p
        .results
        .iter()
        .map(|&i| {
            let a = &app.catalog[i];
            let why = app.reason_for(a.id);
            let style = if why.is_some() { dim() } else { Style::new() };
            ListItem::new(truncate(&format!("{}: {}", a.tab.title(), a.label), w)).style(style)
        })
        .collect();
    let mut state =
        ListState::default().with_selected((!p.results.is_empty()).then_some(p.selected));
    f.render_stateful_widget(
        List::new(items).highlight_style(SELECTED),
        Rect {
            y: inner.y + 1,
            height: inner.height - 1,
            ..inner
        },
        &mut state,
    );
}

fn field_value(fl: &Field, w: usize, focused: bool) -> (String, Option<usize>) {
    match fl.kind {
        FieldKind::Select => {
            let cur = fl.options.get(fl.option).map_or("", String::as_str);
            (
                format!("◀ {cur} ▶  ({}/{})", fl.option + 1, fl.options.len()),
                None,
            )
        }
        FieldKind::Multi => {
            let cur = fl.options.get(fl.option).map_or("", String::as_str);
            let mark = if fl.checked.get(fl.option).copied().unwrap_or(false) {
                "[x]"
            } else {
                "[ ]"
            };
            let n = fl.checked.iter().filter(|c| **c).count();
            (
                format!(
                    "{mark} {cur}  ({}/{}, {n} selected; Space toggles, none = default)",
                    fl.option + 1,
                    fl.options.len()
                ),
                None,
            )
        }
        _ => {
            let (shown, col) = if fl.secret {
                fl.input.masked().window(w)
            } else {
                fl.input.window(w)
            };
            (shown, focused.then_some(col))
        }
    }
}

/// Message lines under a form field: its error (wrapped, up to three lines) or, for a secret
/// field, a hint that the value is hidden. At least one (possibly empty) line.
fn field_notes(fl: &Field, w: usize) -> Vec<(String, bool)> {
    if let Some(err) = &fl.error {
        wrap_text(&sanitize(err), w, 3)
            .into_iter()
            .map(|l| (l, true))
            .collect()
    } else if fl.secret {
        vec![(truncate(SECRET_HINT, w), false)]
    } else {
        vec![(String::new(), false)]
    }
}

fn form(f: &mut Frame<'_>, form: &Form, area: Rect) {
    let width_cells = popup_width(area.width, 76);
    let inner_w = usize::from(width_cells.saturating_sub(2));
    let label_w = form
        .fields
        .iter()
        .map(|x| width(x.label))
        .max()
        .unwrap_or(0)
        .min(inner_w / 2)
        + 2;
    let note_w = inner_w.saturating_sub(label_w).max(1);
    let notes: Vec<Vec<(String, bool)>> = form
        .fields
        .iter()
        .map(|fl| field_notes(fl, note_w))
        .collect();
    let rows: usize = notes.iter().map(|n| 1 + n.len()).sum();
    let h = u16::try_from(rows + 2).unwrap_or(u16::MAX).min(area.height);
    let rect = centered(area, width_cells, h);
    let inner = popup(f, rect, &sanitize(&form.title));
    let value_w = usize::from(inner.width).saturating_sub(label_w);
    let mut y = inner.y;
    for (i, fl) in form.fields.iter().enumerate() {
        if y >= inner.y + inner.height {
            break;
        }
        let focused = i == form.focus;
        let (value, cursor) = field_value(fl, value_w, focused);
        let label_style = if focused {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            dim()
        };
        let label = truncate(fl.label, label_w.saturating_sub(2));
        let line = Line::from(vec![
            Span::styled(format!("{label:<label_w$}"), label_style),
            Span::raw(sanitize(&value)),
        ]);
        f.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
        if let Some(col) = cursor {
            let x = inner.x + u16::try_from(label_w + col).unwrap_or(0);
            f.set_cursor_position(Position::new(x.min(inner.right().saturating_sub(1)), y));
        }
        y += 1;
        for (text, is_error) in notes.get(i).map(Vec::as_slice).unwrap_or_default() {
            if y >= inner.y + inner.height {
                break;
            }
            let style = if *is_error {
                Style::new().fg(Color::Red)
            } else {
                dim()
            };
            f.render_widget(
                Paragraph::new(Line::styled(
                    format!("{}{text}", " ".repeat(label_w)),
                    style,
                )),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y += 1;
        }
    }
}

fn choice(f: &mut Frame<'_>, c: &Choice, area: Rect) {
    let h = u16::try_from(c.items.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2))
        .max(3);
    let rect = centered(area, 44.min(area.width.saturating_sub(2)), h);
    let inner = popup(f, rect, &format!("{} (connector {})", c.title, c.connector));
    let items: Vec<ListItem<'_>> = c.items.iter().map(|i| ListItem::new(i.clone())).collect();
    let mut state = ListState::default().with_selected(Some(c.selected));
    f.render_stateful_widget(
        List::new(items).highlight_style(SELECTED),
        inner,
        &mut state,
    );
}

const HELP: &[&str] = &[
    "First steps: the station connects and boots at startup (the header shows the state; on the",
    "     Station tab x disconnects and c connects again). Press : and type a name",
    "     (BootNotification, Heartbeat, StartTransaction) to run it; tab 7 runs scenarios.",
    "     Everything lands in the log.",
    "",
    "1-7 / Tab / Shift-Tab   switch tab",
    "Up Down j k             move in the focused pane",
    "Left Right              change focused pane (connectors, actions, log)",
    "[ ]                     previous / next connector",
    "Enter                   run action / open log row",
    "letters                 accelerators of the CURRENT tab only (shown in the list)",
    ":  or Ctrl-P            command palette (every action, fuzzy search)",
    "/                       search where there is a search (log pane of the Logs tab, data",
    "                        browser), otherwise the command palette",
    "W                       pending commands with their elapsed time (x cancels, Esc closes)",
    "Esc                     close dialog / clear log search",
    "?                       this help",
    "q or Ctrl-C             quit (asks while a scenario or transaction runs, or in a fleet)",
    "",
    "Station tab: K config keys, L local auth list, P charging profiles, Y certificates",
    "     (data browser: Tab/1-4 page, / filter, Enter edit; a add, i install, d delete,",
    "     c clear); C request certificate, S security event",
    "",
    "Log pane: Enter opens the detail of the selected row on every tab. On the Logs tab (6):",
    "     y copy row, Y copy visible, f filter, s search, p pause/follow, G newest,",
    "     e export JSONL, c clear (asks first)",
    "",
    "Without color (--color never or NO_COLOR): chips and selections are reverse video, errors",
    "     bold, and log lines start with a marker: i info, + success, ! warning, x error.",
];

const FLEET_HELP: &[&str] = &[
    "",
    "Fleet (several stations; everything else acts on the SELECTED station):",
    "< >                     previous / next station",
    "Alt-1 .. Alt-9          jump to station 1..9",
    "Alt-c / Alt-d           connect / disconnect ALL stations",
    "selector row: id, + connected, ~ connecting, - offline, * transaction running",
];

pub fn help_lines(fleet: bool) -> Vec<&'static str> {
    let mut lines = HELP.to_vec();
    if fleet {
        lines.extend(FLEET_HELP);
    }
    lines
}

fn help(f: &mut Frame<'_>, fleet: bool, area: Rect) {
    let rect_w = popup_width(area.width, 96);
    let mut lines = wrap_help(&help_lines(fleet), usize::from(rect_w.saturating_sub(2)));
    if lines.len() + 2 > usize::from(area.height) {
        // A short terminal: drop the spacer lines so more of the help fits.
        lines.retain(|l| !l.is_empty());
    }
    let h = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let rect = centered(area, rect_w, h);
    let inner = popup(f, rect, "Help");
    f.render_widget(
        Paragraph::new(lines.into_iter().map(Line::raw).collect::<Vec<_>>()),
        inner,
    );
}

fn detail(f: &mut Frame<'_>, d: &Detail, area: Rect) {
    let rect = Rect::new(
        area.x + 1.min(area.width / 4),
        area.y + 1.min(area.height / 4),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let inner = popup(f, rect, &sanitize(&d.title));
    let lines: Vec<Line<'_>> = d
        .lines
        .iter()
        .skip(d.scroll)
        .take(usize::from(inner.height))
        .map(|l| Line::raw(l.as_str()))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn confirm(f: &mut Frame<'_>, c: &Confirm, area: Rect) {
    let w = u16::try_from(width(&c.message) + 28)
        .unwrap_or(u16::MAX)
        .max(24);
    let rect = centered(area, w, 3);
    let inner = popup(f, rect, "Confirm");
    let keys = if c.action == ConfirmAction::Quit {
        "[y/q quit, n/Esc stay]"
    } else {
        "[y/N]"
    };
    f.render_widget(Paragraph::new(format!("{}  {keys}", c.message)), inner);
}
