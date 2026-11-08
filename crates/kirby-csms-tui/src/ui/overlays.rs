//! Palette, form, confirmation and results list.

use kirby_ocpp_tui::forms::SECRET_HINT;
use kirby_ocpp_tui::text::{sanitize, truncate, width, wrap_text};
use kirby_ocpp_tui::theme::{Level, level_mark};
use kirby_ocpp_tui::ui::{dim, popup_width};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::{centered, popup};
use crate::actions::{CATALOG, Palette, ResultsList, results_of};
use crate::app::{App, Confirm};
use crate::form::{Field, FieldKind, Form};

const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

pub(super) fn palette(f: &mut Frame<'_>, app: &App, p: &Palette, area: Rect) {
    let h = u16::try_from(p.results.len() + 3)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2))
        .max(4);
    let rect = centered(area, 78.min(area.width.saturating_sub(2)), h);
    let inner = popup(f, rect, "Actions");
    if inner.height < 2 {
        return;
    }
    let w = usize::from(inner.width);
    let (shown, col) = p.query.window(w.saturating_sub(3));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(": ", Style::new().fg(Color::Cyan)),
            Span::raw(sanitize(&shown)),
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
        .filter_map(|&i| CATALOG.get(i))
        .map(|a| {
            let why = app.reason_for(a.id);
            let key = a.key.map_or_else(|| "  ".to_owned(), |k| format!("{k} "));
            let text = match &why {
                Some(why) => format!("{key} {} ({why})", a.label()),
                None => format!("{key} {}", a.label()),
            };
            let style = if why.is_some() { dim() } else { Style::new() };
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

fn field_value(fl: &Field, w: usize, focused: bool, secret: bool) -> (String, Option<usize>) {
    if fl.kind == FieldKind::Select {
        let cur = fl.chosen().unwrap_or("(none)");
        (
            format!("◀ {cur} ▶  ({}/{})", fl.option + 1, fl.options.len()),
            None,
        )
    } else {
        let (shown, col) = if secret {
            fl.input.masked().window(w)
        } else {
            fl.input.window(w)
        };
        (shown, focused.then_some(col))
    }
}

/// Lines under a field: its error wrapped to three lines, or the hint of a hidden value.
fn field_notes(fl: &Field, secret: bool, w: usize) -> Vec<(String, bool)> {
    if let Some(err) = &fl.error {
        wrap_text(&sanitize(err), w, 3)
            .into_iter()
            .map(|l| (l, true))
            .collect()
    } else if secret {
        vec![(truncate(SECRET_HINT, w), false)]
    } else {
        vec![(String::new(), false)]
    }
}

pub(super) fn form(f: &mut Frame<'_>, form: &Form, area: Rect) {
    let width_cells = popup_width(area.width, 90);
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
        .enumerate()
        .map(|(i, fl)| field_notes(fl, form.is_secret(i), note_w))
        .collect();
    let form_error: Vec<String> = form
        .error
        .as_deref()
        .map(|e| wrap_text(&sanitize(e), inner_w.max(1), 3))
        .unwrap_or_default();
    let rows: usize = notes.iter().map(|n| 1 + n.len()).sum::<usize>() + form_error.len();
    let h = u16::try_from(rows + 2).unwrap_or(u16::MAX).min(area.height);
    let rect = centered(area, width_cells, h);
    let inner = popup(f, rect, &sanitize(&form.title));
    let value_w = usize::from(inner.width).saturating_sub(label_w).max(1);
    let mut y = inner.y;
    for (i, fl) in form.fields.iter().enumerate() {
        if y >= inner.y + inner.height {
            break;
        }
        let focused = i == form.focus;
        let (value, cursor) = field_value(fl, value_w, focused, form.is_secret(i));
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
    for text in &form_error {
        if y >= inner.y + inner.height {
            break;
        }
        f.render_widget(
            Paragraph::new(Line::styled(text.clone(), Style::new().fg(Color::Red))),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y += 1;
    }
}

pub(super) fn confirm(f: &mut Frame<'_>, c: &Confirm, area: Rect) {
    let w = u16::try_from(width(&c.message) + 12)
        .unwrap_or(u16::MAX)
        .max(24);
    let rect = centered(area, w, 3);
    let inner = popup(f, rect, "Confirm");
    f.render_widget(
        Paragraph::new(truncate(
            &format!("{}  [y/N]", sanitize(&c.message)),
            usize::from(inner.width),
        )),
        inner,
    );
}

pub(super) fn results(f: &mut Frame<'_>, app: &App, list: &ResultsList, area: Rect) {
    let rows: Vec<_> = results_of(app, &list.station).collect();
    let h = u16::try_from(rows.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2))
        .max(3);
    let rect = centered(area, 90.min(area.width.saturating_sub(2)), h);
    let title = truncate(
        &format!("Results of {}", sanitize(&list.station)),
        usize::from(rect.width.saturating_sub(4)),
    );
    let inner = popup(f, rect, &title);
    let w = usize::from(inner.width);
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|r| {
            let color = if r.ok { Color::Green } else { Color::Red };
            let mark = match (app.mono, r.ok) {
                (false, _) => String::new(),
                (true, true) => format!("{} ", level_mark(Level::Success)),
                (true, false) => format!("{} ", level_mark(Level::Error)),
            };
            ListItem::new(truncate(
                &format!(
                    "{}  {mark}{}: {}",
                    r.at.format("%H:%M:%S"),
                    r.label,
                    r.summary
                ),
                w,
            ))
            .style(Style::new().fg(color))
        })
        .collect();
    let mut state = ListState::default().with_selected((!rows.is_empty()).then_some(list.selected));
    f.render_stateful_widget(
        List::new(items).highlight_style(SELECTED),
        inner,
        &mut state,
    );
}
