//! Single-line text editing and display-width helpers (unicode-correct).

use std::fmt::Write as _;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of `s` in terminal cells.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Characters that must not reach a terminal or a log line: C0/C1 controls (escape sequences,
/// newlines), the Unicode line and paragraph separators and the bidirectional overrides.
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

/// Replaces unsafe characters (see [`is_unsafe_char`]) with a visible dot.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if is_unsafe_char(c) { '·' } else { c })
        .collect()
}

/// Escapes the unsafe characters of one JSON text as `\uXXXX`. serde already escapes the C0
/// controls; this covers the rest (C1, U+2028/2029, bidi), which are legal raw in JSON.
pub fn escape_json_unsafe(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if is_unsafe_char(c) {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Drops the `user:password@` part of a URL so credentials are never displayed or printed.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_owned(),
    }
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
        if is_unsafe_char(c) {
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
            c if is_unsafe_char(c) => None,
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
    fn sanitize_covers_c1_separators_and_bidi() {
        let hostile = "a\u{85}b\u{9b}c\u{2028}d\u{2029}e\u{202e}f\u{2066}g\u{1b}h\n";
        assert_eq!(sanitize(hostile), "a·b·c·d·e·f·g·h·");
        let escaped = escape_json_unsafe("\"a\u{85}\u{202e}b\u{1f600}\"");
        assert_eq!(escaped, "\"a\\u0085\\u202eb\u{1f600}\"");
        assert_eq!(
            serde_json::from_str::<String>(&escaped).unwrap(),
            "a\u{85}\u{202e}b\u{1f600}"
        );
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

#[cfg(test)]
mod redact_tests {
    use super::redact_url;

    #[test]
    fn credentials_are_removed_from_urls() {
        assert_eq!(redact_url("ws://u:p%40w@h:1/x?a=b@c"), "ws://h:1/x?a=b@c");
        assert_eq!(redact_url("wss://tok@h/x"), "wss://h/x");
        assert_eq!(redact_url("ws://h/x@y"), "ws://h/x@y");
        assert_eq!(redact_url("garbage"), "garbage");
    }
}
