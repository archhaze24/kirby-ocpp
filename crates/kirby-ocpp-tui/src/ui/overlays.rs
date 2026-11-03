use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use super::dim;
use crate::app::{App, Confirm, Detail, Mode};
use crate::forms::{Choice, Field, FieldKind, Form};
use crate::palette::Palette;
use crate::text::{sanitize, truncate, width};

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
    }
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
            let (shown, col) = fl.input.window(w);
            (shown, focused.then_some(col))
        }
    }
}

fn form(f: &mut Frame<'_>, form: &Form, area: Rect) {
    let h = u16::try_from(form.fields.len() * 2 + 2).unwrap_or(u16::MAX);
    let rect = centered(area, 76.min(area.width.saturating_sub(2)), h);
    let inner = popup(f, rect, &form.title);
    let label_w = form
        .fields
        .iter()
        .map(|x| width(x.label))
        .max()
        .unwrap_or(0)
        + 2;
    let value_w = usize::from(inner.width).saturating_sub(label_w);
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
        let line = Line::from(vec![
            Span::styled(format!("{:<label_w$}", fl.label), label_style),
            Span::raw(sanitize(&value)),
        ]);
        f.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
        if let Some(col) = cursor {
            f.set_cursor_position(Position::new(
                inner.x + u16::try_from(label_w + col).unwrap_or(0),
                y,
            ));
        }
        if let Some(err) = &fl.error
            && y + 1 < inner.y + inner.height
        {
            f.render_widget(
                Paragraph::new(Line::styled(
                    truncate(
                        &format!("{}{}", " ".repeat(label_w), sanitize(err)),
                        usize::from(inner.width),
                    ),
                    Style::new().fg(Color::Red),
                )),
                Rect::new(inner.x, y + 1, inner.width, 1),
            );
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
    "1-7 / Tab / Shift-Tab   switch tab",
    "Up Down j k             move in the focused pane",
    "Left Right              change focused pane (connectors, actions, log)",
    "[ ]                     previous / next connector",
    "Enter                   run action / open log row",
    "letters                 accelerators of the CURRENT tab only (shown in the list)",
    "/ or :                  command palette (search log when in the log pane)",
    "Esc                     close dialog / clear log search",
    "?                       this help",
    "q or Ctrl-C             quit",
    "",
    "Station tab: K config keys, L local auth list, P charging profiles, Y certificates",
    "     (data browser: Tab/1-4 page, / filter, Enter edit; a add, i install, d delete,",
    "     c clear); C request certificate, S security event",
    "",
    "Log: Enter detail, y copy row, Y copy visible, f filter, s search,",
    "     p pause/follow, G newest, e export JSONL, c clear (asks first)",
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
    let lines = help_lines(fleet);
    let h = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let rect = centered(area, 80.min(area.width.saturating_sub(2)), h);
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
    let w = u16::try_from(width(&c.message) + 12)
        .unwrap_or(u16::MAX)
        .max(24);
    let rect = centered(area, w, 3);
    let inner = popup(f, rect, "Confirm");
    f.render_widget(Paragraph::new(format!("{}  [y/N]", c.message)), inner);
}
