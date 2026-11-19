//! Monochrome rendering: the terminal UIs stay readable without color.
//!
//! Colors carry no information on their own: every state is also a word or a marker in the
//! text. In monochrome mode ([`apply_monochrome`]) the remaining color cues become text
//! modifiers: filled chips and selections are reversed, red and yellow text is bold, and every
//! color is cleared.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

/// Palette: secondary text (labels, hints, inactive items, idle markers). ANSI 7, never the
/// bright-black ANSI 8, which is barely readable on common dark themes.
pub const SECONDARY: Color = Color::Gray;

/// Palette: the focus accent (focused pane border, popup border).
pub const ACCENT: Color = Color::Cyan;

/// Secondary text style. Not dimmed: it has to stay readable.
#[must_use]
pub const fn secondary() -> Style {
    Style::new().fg(SECONDARY)
}

/// Pane border: the accent when focused, secondary otherwise.
#[must_use]
pub const fn border(focused: bool) -> Style {
    Style::new().fg(if focused { ACCENT } else { SECONDARY })
}

/// Rewrites every cell of a drawn frame so that it carries no color.
///
/// - a colored background (status chips, the active tab) becomes reverse video;
/// - red, yellow and cyan text (errors, warnings, the focused pane) becomes bold;
/// - foreground and background are reset.
pub fn apply_monochrome(buf: &mut Buffer) {
    for cell in &mut buf.content {
        let mut add = Modifier::empty();
        if cell.bg != Color::Reset && !cell.modifier.contains(Modifier::REVERSED) {
            add |= Modifier::REVERSED;
        }
        match cell.fg {
            Color::Red | Color::Yellow | Color::LightRed | Color::LightYellow | Color::Cyan => {
                add |= Modifier::BOLD;
            }
            _ => {}
        }
        // A chip draws black text on a colored fill: reversed video already swaps them.
        cell.modifier |= add;
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
    }
}

/// How serious a line is, for the marker drawn in front of it in monochrome mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warn,
    Error,
}

/// The one-character marker of a level (monochrome mode only).
#[must_use]
pub fn level_mark(level: Level) -> &'static str {
    match level {
        Level::Info => "i",
        Level::Success => "+",
        Level::Warn => "!",
        Level::Error => "x",
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::*;

    #[test]
    fn colors_become_modifiers() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        buf[(0, 0)].set_style(Style::new().fg(Color::Black).bg(Color::Green));
        buf[(1, 0)].set_style(Style::new().fg(Color::Red));
        buf[(2, 0)].set_style(secondary());
        buf[(3, 0)].set_style(Style::new().fg(Color::Blue));
        apply_monochrome(&mut buf);
        assert!(buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert!(buf[(1, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(2, 0)].modifier, Modifier::empty());
        assert_eq!(buf[(3, 0)].modifier, Modifier::empty());
        for cell in &buf.content {
            assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset));
        }
    }

    #[test]
    fn a_selection_stays_reversed_once() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        buf[(0, 0)].set_style(
            Style::new()
                .bg(Color::Cyan)
                .add_modifier(Modifier::REVERSED),
        );
        apply_monochrome(&mut buf);
        assert!(buf[(0, 0)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn level_marks_differ() {
        let marks = [Level::Info, Level::Success, Level::Warn, Level::Error].map(level_mark);
        for (i, a) in marks.iter().enumerate() {
            for b in &marks[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
