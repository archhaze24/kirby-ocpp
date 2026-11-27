use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use super::{dim, popup_width};
use crate::actions::Tab;
use crate::app::{App, ConfirmAction, Detail, Mode};
use crate::forms::{Choice, Field, FieldKind, Form, SECRET_HINT};
use crate::help::{HelpSection, row};
use crate::palette::Palette;
use crate::text::{sanitize, truncate, width, wrap_text};

pub(crate) const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

pub(crate) fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// The accelerator column of a palette row: the key, or blanks of the same width.
pub(crate) fn key_column(key: Option<char>) -> String {
    key.map_or_else(|| "   ".to_owned(), |k| format!("{k}  "))
}

pub(crate) fn popup(f: &mut Frame<'_>, rect: Rect, title: &str) -> Rect {
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
        Mode::Palette(p) => palette(f, p, area, |i| {
            let a = &app.catalog[i];
            // A letter works on its own tab only, so other rows show none.
            let key = if a.tab == app.tab { a.key } else { None };
            (
                format!("{}{}: {}", key_column(key), a.tab.title(), a.label),
                app.reason_for(a.id).is_some(),
            )
        }),
        Mode::Form(form) => self::form(f, form, area),
        Mode::Choice(c) => choice(f, c, area),
        Mode::Help(state) => crate::help::draw(f, area, &help_sections(app), state),
        Mode::Detail(d) => detail(f, d, area, &app.details.popup_max),
        Mode::Confirm(c) => confirm(f, &c.message, c.action == ConfirmAction::Quit, area),
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

/// The command palette. `row` gives the list text of the catalogue entry at an index of
/// `p.results` and whether the action is unavailable (drawn dim).
pub(crate) fn palette(
    f: &mut Frame<'_>,
    p: &Palette,
    area: Rect,
    row: impl Fn(usize) -> (String, bool),
) {
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
    let mut prompt = vec![
        Span::styled("/ ", Style::new().fg(Color::Cyan)),
        Span::raw(shown),
    ];
    if p.query.is_empty() {
        prompt.push(Span::styled("type to filter", dim()));
    }
    f.render_widget(
        Paragraph::new(Line::from(prompt)),
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
            let (text, unavailable) = row(i);
            let style = if unavailable { dim() } else { Style::new() };
            ListItem::new(truncate(&text, w)).style(style)
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
            let mut input = if fl.secret {
                fl.input.masked()
            } else {
                fl.input.clone()
            };
            if fl.fresh {
                // An untouched prefill opens at its start: a long value shows its beginning.
                input.home();
            }
            let (shown, col) = input.window(w);
            // The next key replaces a fresh value, so it has no cursor to show.
            (shown, (focused && !fl.fresh).then_some(col))
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

pub(crate) fn form(f: &mut Frame<'_>, form: &Form, area: Rect) {
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
        // A prefilled value that the next key replaces is drawn as selected.
        let value_style = if fl.fresh && focused && fl.is_text() && !value.is_empty() {
            SELECTED
        } else {
            Style::new()
        };
        let line = Line::from(vec![
            Span::styled(format!("{label:<label_w$}"), label_style),
            Span::styled(sanitize(&value), value_style),
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

pub(crate) fn choice(f: &mut Frame<'_>, c: &Choice, area: Rect) {
    let h = u16::try_from(c.items.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2))
        .max(3);
    let rect = centered(area, 44.min(area.width.saturating_sub(2)), h);
    let title = if c.target_label.is_empty() {
        c.title.clone()
    } else {
        format!("{} ({} {})", c.title, c.target_label, c.target)
    };
    let inner = popup(f, rect, &title);
    let items: Vec<ListItem<'_>> = c.items.iter().map(|i| ListItem::new(i.clone())).collect();
    let mut state = ListState::default().with_selected(Some(c.selected));
    f.render_stateful_widget(
        List::new(items).highlight_style(SELECTED),
        inner,
        &mut state,
    );
}

/// The "Everywhere" section of the emulator help. `tabs` is the range of tab digits, `item`
/// what the left list holds (`connector`, `EVSE`), `logs` the tab number of the Logs tab.
pub(crate) fn help_everywhere(tabs: &str, item: &str, logs: usize, fleet: bool) -> HelpSection {
    let mut lines = vec![
        row(tabs, "Switch tab (Tab and Shift-Tab step through them)"),
        row(
            "←/→",
            &format!("Move between the panes: {item}s, details, actions, log"),
        ),
        row("↑/↓ j/k", "Move, or scroll the details"),
        row(
            "PgUp/PgDn",
            "A page up or down; Home and End jump to the first and last row",
        ),
        row("[ ]", &format!("Previous or next {item}")),
        row(
            "Enter",
            "Run the selected action, or open the selected log row",
        ),
        row(": Ctrl-P", "Command palette: every action, type to filter"),
        row("W", "Commands waiting for the CSMS; x cancels one"),
        row(
            "/",
            &format!(
                "Search in the log of the Logs tab ({logs}) and in the data browsers; elsewhere it opens the palette"
            ),
        ),
        row(
            "z",
            "Zoom the focused pane to the whole body; z or Esc goes back",
        ),
        row("?", "This help"),
        row(
            "q",
            "Quit; it asks first while a scenario or a transaction runs, or in a fleet",
        ),
    ];
    if fleet {
        lines.extend([
            row("< >", "Previous or next station"),
            row("Alt-1..9", "Jump to a station"),
            row("Alt-c/d", "Connect or disconnect every station"),
            row(
                "",
                "Strip markers: ● connected, ◐ connecting, ○ offline, * a transaction runs",
            ),
        ]);
    }
    lines.push(row(
        "",
        "Without color (--color never or NO_COLOR): reverse video and bold, and log lines start with i info, + success, ! warning, x error.",
    ));
    HelpSection::new("Everywhere", lines)
}

/// Rows `key  label` of the actions of one tab, from the catalogue.
pub(crate) fn tab_rows<'a>(actions: impl Iterator<Item = (Option<char>, &'a str)>) -> Vec<String> {
    actions
        // An action without a letter is run from the list or the palette; the pending list
        // is `W`, which the "Everywhere" section names.
        .filter_map(|(key, label)| Some(row(&key?.to_string(), &sanitize(label))))
        .collect()
}

/// The sections of the emulator help for OCPP 1.6: the current tab first.
pub fn help_sections(app: &App) -> Vec<HelpSection> {
    let mut lines = Vec::new();
    match app.tab {
        Tab::Station => lines.push(row(
            "",
            "The station connects and boots at startup; the header shows the state.",
        )),
        Tab::Connector => lines.push(row(
            "",
            "Plug, status and faults of the selected connector ([ ] picks another).",
        )),
        Tab::Transaction => lines.push(row(
            "Flow",
            "p plug → a authorize → t start/present → m meter → e stop (p is on the Connector tab)",
        )),
        Tab::Maintenance => lines.push(row(
            "",
            "Choose how the next diagnostics upload or firmware update ends, or send their status.",
        )),
        Tab::Data => lines.push(row(
            "",
            "Vendor messages and raw CALLs checked against the OCPP schema.",
        )),
        Tab::Logs => lines.push(row(
            "",
            "The log pane has the keys; Enter opens the detail of the selected row.",
        )),
        Tab::Scenarios => lines.push(row(
            "",
            "Run a built-in scenario or a scenario file; the Scenario pane shows the steps.",
        )),
    }
    lines.extend(tab_rows(
        app.catalog
            .iter()
            .filter(|a| a.tab == app.tab)
            .map(|a| (a.key, a.label.as_str())),
    ));
    if app.tab == Tab::Station {
        lines.push(row(
            "",
            "The K, L, P and Y browsers: Tab or 1-4 changes the page, / filters, Enter edits; a add, i install, d delete, c clear.",
        ));
    }
    vec![
        HelpSection::new("This tab", lines),
        help_everywhere("1-7", "connector", Tab::Logs.index() + 1, app.in_fleet),
    ]
}

/// Height of a popup that shows `lines` rows: the content plus borders, at most the free
/// area (a one-row margin above and below).
pub fn content_height(area_height: u16, lines: usize) -> u16 {
    let want = u16::try_from(lines.saturating_add(2)).unwrap_or(u16::MAX);
    want.min(area_height.saturating_sub(2)).max(3)
}

/// The first row to draw: `scroll`, but never past the point where the last row would leave
/// the popup half empty.
pub fn visible_scroll(scroll: usize, lines: usize, rows: u16) -> usize {
    scroll.min(lines.saturating_sub(usize::from(rows)))
}

pub(crate) fn detail(
    f: &mut Frame<'_>,
    d: &Detail,
    area: Rect,
    max_scroll: &std::cell::Cell<usize>,
) {
    let w = area.width.saturating_sub(2);
    let rect = centered(area, w, content_height(area.height, d.lines.len()));
    let inner = popup(f, rect, &sanitize(&d.title));
    max_scroll.set(d.lines.len().saturating_sub(usize::from(inner.height)));
    let start = visible_scroll(d.scroll, d.lines.len(), inner.height);
    let lines: Vec<Line<'_>> = d
        .lines
        .iter()
        .skip(start)
        .take(usize::from(inner.height))
        .map(|l| Line::raw(l.as_str()))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// A yes/no question; `quit` shows the keys of the quit question (`y`/`q` quit, `n`/Esc stay).
pub(crate) fn confirm(f: &mut Frame<'_>, message: &str, quit: bool, area: Rect) {
    let w = u16::try_from(width(message) + 28)
        .unwrap_or(u16::MAX)
        .max(24);
    let rect = centered(area, w, 3);
    let inner = popup(f, rect, "Confirm");
    let keys = if quit {
        "[y/q quit, n/Esc stay]"
    } else {
        "[y/N]"
    };
    f.render_widget(Paragraph::new(format!("{message}  {keys}")), inner);
}
