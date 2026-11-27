//! The help popup shared by the emulator and the CSMS: titled sections, one visible at a
//! time, scrollable.
//!
//! A section is a list of rows written with [`row`] (`key  description`) or plain notes; long
//! rows wrap under the description column. `←`/`→` switch the section, `↑`/`↓` scroll, `Esc`
//! or `?` close.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::text::{truncate, width, wrap_help, wrap_text};
use crate::ui::overlays::{centered, popup};
use crate::ui::{dim, more_above, more_below, popup_width};

/// Width of the popup on a wide terminal.
const WIDTH: u16 = 100;
/// Rows a page key moves before the popup has been drawn once.
const DEFAULT_PAGE: usize = 8;

/// One titled part of the help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSection {
    pub title: String,
    pub lines: Vec<String>,
}

impl HelpSection {
    pub fn new<S: Into<String>>(
        title: impl Into<String>,
        lines: impl IntoIterator<Item = S>,
    ) -> Self {
        Self {
            title: title.into(),
            lines: lines.into_iter().map(Into::into).collect(),
        }
    }
}

/// A `key  description` row. The description wraps under its own column. An empty `key`
/// gives a plain note that wraps to the full width.
#[must_use]
pub fn row(key: &str, text: &str) -> String {
    if key.is_empty() {
        text.to_owned()
    } else {
        format!("{key:<11}  {text}")
    }
}

/// Whether `line` is a `key  description` row made by [`row`] (the description starts in the
/// fixed column after two spaces) and not a note.
fn is_keyed(line: &str) -> bool {
    let mut cells = line.chars().skip(11);
    line.chars().next().is_some_and(|c| c != ' ')
        && cells.next() == Some(' ')
        && cells.next() == Some(' ')
        && cells.next().is_some_and(|c| c != ' ')
}

/// A row wraps under its description column, a note to the full width.
fn wrap_line(line: &str, w: usize) -> Vec<String> {
    if is_keyed(line) {
        wrap_help(&[line], w)
    } else {
        wrap_text(line, w, usize::MAX)
    }
}

/// Which section is open and how far it is scrolled.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelpState {
    pub section: usize,
    pub scroll: usize,
    /// Filled by the last draw: rows the body shows and the largest useful scroll. The key
    /// handler cannot know the terminal size, so it clamps with what was drawn.
    view: Cell<View>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct View {
    rows: usize,
    max_scroll: usize,
}

impl Default for View {
    fn default() -> Self {
        Self {
            rows: DEFAULT_PAGE,
            max_scroll: usize::MAX,
        }
    }
}

/// What a key did to the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpEvent {
    Stay,
    Close,
}

impl HelpState {
    /// A state that opens on `section`.
    #[must_use]
    pub fn at(section: usize) -> Self {
        Self {
            section,
            ..Self::default()
        }
    }

    /// Applies `key`; `sections` is how many sections the popup has.
    pub fn handle_key(&mut self, key: KeyEvent, sections: usize) -> HelpEvent {
        let view = self.view.get();
        let max = view.max_scroll;
        let page = view.rows.saturating_sub(1).max(1);
        self.scroll = self.scroll.min(max);
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return HelpEvent::Stay;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('?' | 'q') => return HelpEvent::Close,
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(max),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(page).min(max),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = max,
            KeyCode::Left | KeyCode::BackTab => self.switch(sections, -1),
            KeyCode::Right | KeyCode::Tab => self.switch(sections, 1),
            _ => {}
        }
        HelpEvent::Stay
    }

    fn switch(&mut self, sections: usize, d: isize) {
        if sections == 0 {
            return;
        }
        let next = self.section.min(sections - 1).saturating_add_signed(d);
        let next = next.min(sections - 1);
        if next != self.section {
            self.section = next;
            self.scroll = 0;
            self.view.set(View::default());
        }
    }
}

/// Draws the help popup over `area`.
pub fn draw(f: &mut Frame<'_>, area: Rect, sections: &[HelpSection], state: &HelpState) {
    if sections.is_empty() || area.width < 8 || area.height < 5 {
        return;
    }
    let rect_w = popup_width(area.width, WIDTH);
    // One cell of padding after the border.
    let text_w = usize::from(rect_w.saturating_sub(3));
    let wrapped: Vec<Vec<String>> = sections
        .iter()
        .map(|s| s.lines.iter().flat_map(|l| wrap_line(l, text_w)).collect())
        .collect();
    // The popup keeps its height when the section changes.
    let tallest = wrapped.iter().map(Vec::len).max().unwrap_or(0);
    let room = if area.height > 12 {
        area.height - 2
    } else {
        area.height
    };
    let h = u16::try_from(tallest + 4).unwrap_or(u16::MAX).min(room);
    let rect = centered(area, rect_w, h);
    let inner = popup(f, rect, "Help");
    if inner.height < 3 {
        return;
    }
    let idx = state.section.min(sections.len() - 1);
    let lines = &wrapped[idx];
    let rows = usize::from(inner.height - 2);
    let max_scroll = lines.len().saturating_sub(rows);
    state.view.set(View { rows, max_scroll });
    let start = state.scroll.min(max_scroll);
    let body: Vec<Line<'_>> = lines
        .iter()
        .skip(start)
        .take(rows)
        .map(|l| Line::raw(format!(" {l}")))
        .collect();
    let w = usize::from(inner.width);
    f.render_widget(
        Paragraph::new(section_strip(sections, idx, w)),
        Rect { height: 1, ..inner },
    );
    f.render_widget(
        Paragraph::new(body),
        Rect {
            y: inner.y + 1,
            height: inner.height - 2,
            ..inner
        },
    );
    f.render_widget(
        Paragraph::new(footer(
            start,
            lines.len().saturating_sub(start + rows),
            sections.len(),
            w,
        )),
        Rect {
            y: inner.y + inner.height - 1,
            height: 1,
            ..inner
        },
    );
}

/// The section titles, the open one highlighted. When they do not all fit, only the open
/// one shows, with its place among the sections.
fn section_strip(sections: &[HelpSection], open: usize, w: usize) -> Line<'static> {
    let active = Style::new()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let all: usize = sections.iter().map(|s| width(&s.title) + 3).sum();
    if all <= w {
        let spans: Vec<Span<'static>> = sections
            .iter()
            .enumerate()
            .flat_map(|(i, s)| {
                let style = if i == open { active } else { Style::new() };
                [
                    Span::styled(format!(" {} ", s.title), style),
                    Span::raw(" "),
                ]
            })
            .collect();
        return Line::from(spans);
    }
    let place = format!(" {}/{}", open + 1, sections.len());
    let title = truncate(
        &sections[open].title,
        w.saturating_sub(width(&place) + 2).max(1),
    );
    Line::from(vec![
        Span::styled(format!(" {title} "), active),
        Span::styled(place, dim()),
    ])
}

/// The bottom row: how much is hidden above and below, and the keys.
fn footer(above: usize, below: usize, sections: usize, w: usize) -> Line<'static> {
    let mut marks = Vec::new();
    if above > 0 {
        marks.push(more_above(above));
    }
    if below > 0 {
        marks.push(more_below(below));
    }
    let marks = marks.join("  ");
    let keys = if sections > 1 {
        "←→ section  Esc close"
    } else {
        "Esc close"
    };
    let mut text = marks.clone();
    for k in [keys, "Esc close"] {
        let gap = if marks.is_empty() { 0 } else { 2 };
        if width(&marks) + gap + width(k) <= w {
            let pad = w - width(&marks) - width(k);
            text = format!("{marks}{}{k}", " ".repeat(pad));
            break;
        }
    }
    Line::styled(truncate(&text, w), dim())
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sections() -> Vec<HelpSection> {
        vec![
            HelpSection::new(
                "This tab",
                (1..=30).map(|i| row(&format!("k{i}"), &format!("does thing {i}"))),
            ),
            HelpSection::new("Everywhere", [row("?", "help"), row("q", "quit")]),
        ]
    }

    fn render(w: u16, h: u16, state: &HelpState) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, f.area(), &sections(), state)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn rows_align_the_description_after_the_key() {
        assert_eq!(row("Up/Down", "move"), "Up/Down      move");
    }

    #[test]
    fn scrolling_stops_at_the_ends_and_shows_markers() {
        let mut s = HelpState::default();
        let top = render(80, 24, &s);
        assert!(top.contains("▼ ") && top.contains("more"), "{top}");
        assert!(!top.contains('▲'), "{top}");
        assert!(top.contains("k1 ") && !top.contains("k30"), "{top}");
        for _ in 0..100 {
            s.handle_key(key(KeyCode::Down), 2);
        }
        let end = render(80, 24, &s);
        assert!(
            end.contains("k30") && end.contains('▲') && !end.contains('▼'),
            "{end}"
        );
        // One key up scrolls at once: the position never ran past the end.
        s.handle_key(key(KeyCode::Up), 2);
        assert_eq!(s.scroll, s.view.get().max_scroll - 1);
        s.handle_key(key(KeyCode::Home), 2);
        assert_eq!(s.scroll, 0);
        s.handle_key(key(KeyCode::End), 2);
        assert_eq!(s.scroll, s.view.get().max_scroll);
    }

    #[test]
    fn page_keys_move_a_page() {
        let mut s = HelpState::default();
        render(80, 24, &s);
        let rows = s.view.get().rows;
        s.handle_key(key(KeyCode::PageDown), 2);
        assert_eq!(s.scroll, (rows - 1).min(s.view.get().max_scroll));
        assert!(s.scroll > 0);
        s.handle_key(key(KeyCode::PageUp), 2);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn arrows_switch_sections_and_reset_the_scroll() {
        let mut s = HelpState::default();
        render(80, 24, &s);
        s.handle_key(key(KeyCode::Down), 2);
        assert_eq!(s.handle_key(key(KeyCode::Right), 2), HelpEvent::Stay);
        assert_eq!((s.section, s.scroll), (1, 0));
        let text = render(80, 24, &s);
        assert!(text.contains("quit") && !text.contains("k1 "), "{text}");
        s.handle_key(key(KeyCode::Right), 2);
        assert_eq!(s.section, 1, "no wrap past the last section");
        s.handle_key(key(KeyCode::Left), 2);
        assert_eq!(s.section, 0);
    }

    #[test]
    fn escape_and_question_mark_close() {
        for code in [KeyCode::Esc, KeyCode::Char('?')] {
            assert_eq!(
                HelpState::default().handle_key(key(code), 2),
                HelpEvent::Close
            );
        }
        for code in [KeyCode::Char('x'), KeyCode::Char('3'), KeyCode::Enter] {
            assert_eq!(
                HelpState::default().handle_key(key(code), 2),
                HelpEvent::Stay
            );
        }
    }

    #[test]
    fn fits_a_tiny_terminal() {
        let s = HelpState::default();
        let text = render(24, 8, &s);
        assert!(text.contains("Help"), "{text}");
        assert!(text.contains("This tab"), "{text}");
        assert!(text.contains("more"), "{text}");
        for line in text.lines() {
            assert!(width(line) <= 24, "{line}");
        }
    }

    #[test]
    fn long_rows_wrap_under_the_description() {
        let text = [HelpSection::new(
            "This tab",
            [row("t", &"word ".repeat(40))],
        )];
        let mut t = Terminal::new(TestBackend::new(60, 24)).unwrap();
        t.draw(|f| draw(f, f.area(), &text, &HelpState::default()))
            .unwrap();
        let buf = t.backend().buffer().clone();
        let rows: Vec<String> = (0..24)
            .map(|y| (0..60).map(|x| buf[(x, y)].symbol().to_owned()).collect())
            .collect();
        let first = rows.iter().position(|r| r.contains("t  ")).unwrap();
        assert!(
            rows[first + 1].starts_with("│              word"),
            "{rows:#?}"
        );
    }
}
