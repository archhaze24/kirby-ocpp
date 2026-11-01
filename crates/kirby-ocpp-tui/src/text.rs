//! Single-line text editing and display-width helpers (unicode-correct).

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of `s` in terminal cells.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Replaces control characters (which could drive the terminal) with a visible dot.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '·' } else { c })
        .collect()
}

/// Cuts `s` to at most `max` cells, ending in `…` when something was dropped.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let w = width(g);
        if used + w + 1 > max {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

/// A one-line editable string with a grapheme-aware cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    /// Byte offset, always on a grapheme boundary.
    cursor: usize,
}

impl TextInput {
    pub fn new(text: impl Into<String>) -> Self {
        let text = sanitize_line(&text.into());
        let cursor = text.len();
        Self { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor_byte(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Inserts pasted text; newlines and tabs become spaces, other control chars are dropped.
    pub fn insert_str(&mut self, s: &str) {
        let clean = sanitize_line(s);
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    pub fn backspace(&mut self) {
        if let Some((start, _)) = self.text[..self.cursor].grapheme_indices(true).next_back() {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
    }

    pub fn delete(&mut self) {
        if let Some(g) = self.text[self.cursor..].graphemes(true).next() {
            let end = self.cursor + g.len();
            self.text.replace_range(self.cursor..end, "");
        }
    }

    pub fn left(&mut self) {
        if let Some((start, _)) = self.text[..self.cursor].grapheme_indices(true).next_back() {
            self.cursor = start;
        }
    }

    pub fn right(&mut self) {
        if let Some(g) = self.text[self.cursor..].graphemes(true).next() {
            self.cursor += g.len();
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Number of characters (not bytes, not graphemes).
    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// The part of the text that fits in `max` cells while keeping the cursor visible,
    /// and the cursor column inside it.
    pub fn window(&self, max: usize) -> (String, usize) {
        if max == 0 {
            return (String::new(), 0);
        }
        let before: Vec<&str> = self.text[..self.cursor].graphemes(true).collect();
        let after: Vec<&str> = self.text[self.cursor..].graphemes(true).collect();
        let mut start = 0;
        let mut cursor_col: usize = before.iter().map(|g| width(g)).sum();
        while cursor_col >= max && start < before.len() {
            cursor_col -= width(before[start]);
            start += 1;
        }
        let mut out = String::new();
        let mut used = 0;
        for g in before[start..].iter().chain(after.iter()) {
            let w = width(g);
            if used + w > max {
                break;
            }
            out.push_str(g);
            used += w;
        }
        (out, cursor_col)
    }
}

fn sanitize_line(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// Cell width of a char (0 for combining marks).
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_by_grapheme() {
        let mut t = TextInput::new("a👍🏽b");
        t.left();
        t.backspace();
        assert_eq!(t.text(), "ab");
        t.home();
        t.delete();
        assert_eq!(t.text(), "b");
        t.end();
        t.insert_char('é');
        assert_eq!(t.text(), "bé");
        assert_eq!(t.char_count(), 2);
    }

    #[test]
    fn paste_flattens_newlines_and_drops_controls() {
        let mut t = TextInput::default();
        t.insert_str("a\nb\x1b[31m\tc");
        assert_eq!(t.text(), "a b[31m c");
    }

    #[test]
    fn window_keeps_cursor_visible_with_wide_chars() {
        let t = TextInput::new("日本語日本語");
        let (shown, col) = t.window(6);
        assert!(width(&shown) <= 6);
        assert!(col < 6);
        let mut t = TextInput::new("日本語");
        t.home();
        assert_eq!(t.window(4), ("日本".to_owned(), 0));
    }

    #[test]
    fn truncate_counts_cells() {
        assert_eq!(truncate("日本語", 5), "日本…");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 0), "");
    }
}
