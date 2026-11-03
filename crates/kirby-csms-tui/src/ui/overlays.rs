//! Palette, form, confirmation and results list.

use kirby_ocpp_tui::text::{sanitize, truncate, width};
use kirby_ocpp_tui::ui::dim;
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
                Some(why) => format!("{key} {} ({why})", a.label),
                None => format!("{key} {}", a.label),
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

fn field_value(fl: &Field, w: usize, focused: bool) -> (String, Option<usize>) {
    if fl.kind == FieldKind::Select {
        let cur = fl.chosen().unwrap_or("(none)");
        (
            format!("◀ {cur} ▶  ({}/{})", fl.option + 1, fl.options.len()),
            None,
        )
    } else {
        let (shown, col) = fl.input.window(w);
        (shown, focused.then_some(col))
    }
}

pub(super) fn form(f: &mut Frame<'_>, form: &Form, area: Rect) {
    let h = u16::try_from(form.fields.len() * 2 + 3).unwrap_or(u16::MAX);
    let rect = centered(
        area,
        90.min(area.width.saturating_sub(2)),
        h.min(area.height),
    );
    let inner = popup(f, rect, &sanitize(&form.title));
    let label_w = form
        .fields
        .iter()
        .map(|x| width(x.label))
        .max()
        .unwrap_or(0)
        .min(usize::from(inner.width) / 2)
        + 2;
    let value_w = usize::from(inner.width).saturating_sub(label_w).max(1);
    for (i, fl) in form.fields.iter().enumerate() {
        let y = inner.y + u16::try_from(i * 2).unwrap_or(0);
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
        if let Some(err) = &fl.error
            && y + 1 < inner.y + inner.height
        {
            error_line(f, err, label_w, Rect::new(inner.x, y + 1, inner.width, 1));
        }
    }
    if let Some(err) = &form.error {
        let y = inner.y + u16::try_from(form.fields.len() * 2).unwrap_or(0);
        if y < inner.y + inner.height {
            error_line(f, err, 0, Rect::new(inner.x, y, inner.width, 1));
        }
    }
}

fn error_line(f: &mut Frame<'_>, err: &str, indent: usize, rect: Rect) {
    f.render_widget(
        Paragraph::new(Line::styled(
            truncate(
                &format!("{}{}", " ".repeat(indent), sanitize(err)),
                usize::from(rect.width),
            ),
            Style::new().fg(Color::Red),
        )),
        rect,
    );
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
            ListItem::new(truncate(
                &format!("{}  {}: {}", r.at.format("%H:%M:%S"), r.label, r.summary),
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
