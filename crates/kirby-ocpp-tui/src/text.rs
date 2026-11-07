//! Single-line text editing and display-width helpers (unicode-correct).

use std::fmt::Write as _;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of `s` in terminal cells.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub use kirby_ocpp_core::text::is_unsafe_char;

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

/// Cuts `s` to at most `max` cells by dropping the middle, so the head and the tail stay
/// visible (`LLLL…éé`). Short texts are returned as they are.
pub fn truncate_middle(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_owned();
    }
    let budget = max - 1;
    let head_budget = budget.div_ceil(2);
    let tail_budget = budget - head_budget;
    let mut head = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let w = width(g);
        if used + w > head_budget {
            break;
        }
        head.push_str(g);
        used += w;
    }
    let mut tail: Vec<&str> = Vec::new();
    let mut used = 0;
    for g in s.graphemes(true).rev() {
        let w = width(g);
        if used + w > tail_budget {
            break;
        }
        tail.push(g);
        used += w;
    }
    tail.reverse();
    format!("{head}…{}", tail.concat())
}

/// Wraps help text to `max` cells. A line that starts with five or more spaces continues the
/// paragraph above it. Continuation lines of a wrapped paragraph are indented under the
/// description column (`key   description` rows) or by five spaces. Runs of spaces inside a
/// line are kept, except at a break. No output line is wider than `max`.
pub fn wrap_help(lines: &[&str], max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut paragraphs: Vec<String> = Vec::new();
    for line in lines {
        let continues =
            line.starts_with("     ") && paragraphs.last().is_some_and(|p| !p.is_empty());
        if continues {
            if let Some(last) = paragraphs.last_mut() {
                last.push(' ');
                last.push_str(line.trim());
            }
        } else {
            paragraphs.push((*line).to_owned());
        }
    }
    let mut out = Vec::new();
    for p in &paragraphs {
        wrap_paragraph(p, max, hang_indent(p).min(max / 2), &mut out);
    }
    out
}

fn hang_indent(p: &str) -> usize {
    let mut seen_word = false;
    let mut gap = 0;
    for (col, c) in p.chars().enumerate().take(30) {
        if c == ' ' {
            gap += 1;
        } else {
            if seen_word && gap >= 2 {
                return col;
            }
            seen_word = true;
            gap = 0;
        }
    }
    5
}

/// Wraps one message (an error, a notice) to `max` cells and at most `max_lines` lines; the
/// last line ends in `…` when text was dropped. Control characters must be sanitized first.
pub fn wrap_text(s: &str, max: usize, max_lines: usize) -> Vec<String> {
    let mut out = Vec::new();
    wrap_paragraph(s.trim(), max.max(1), 0, &mut out);
    if out.len() > max_lines.max(1) {
        out.truncate(max_lines.max(1));
        if let Some(last) = out.last_mut() {
            let cut = truncate(&format!("{last} …"), max.max(1));
            *last = if cut.ends_with('…') {
                cut
            } else {
                format!("{cut}…")
            };
        }
    }
    out
}

fn wrap_paragraph(p: &str, max: usize, hang: usize, out: &mut Vec<String>) {
    if p.is_empty() {
        out.push(String::new());
        return;
    }
    let start = out.len();
    let mut line = String::new();
    let mut line_w = 0;
    let mut gap = 0;
    let mut word = String::new();
    let mut tokens: Vec<(usize, String)> = Vec::new();
    for c in p.chars().chain(std::iter::once(' ')) {
        if c == ' ' {
            if !word.is_empty() {
                tokens.push((gap, std::mem::take(&mut word)));
                gap = 0;
            }
            gap += 1;
        } else {
            word.push(c);
        }
    }
    for (gap, word) in tokens {
        let mut word = word;
        let ww = width(&word);
        let need = gap + ww;
        if line_w + need <= max {
            line.push_str(&" ".repeat(gap));
            line_w += gap;
            line.push_str(&word);
            line_w += ww;
            continue;
        }
        if !line.trim().is_empty() {
            out.push(std::mem::take(&mut line));
        }
        let indent = if out.len() == start { 0 } else { hang };
        line = " ".repeat(indent);
        line_w = indent;
        while width(&word) > max - line_w {
            let room = max - line_w;
            let mut piece = String::new();
            let mut used = 0;
            let mut rest = String::new();
            for g in word.graphemes(true) {
                let w = width(g);
                if rest.is_empty() && used + w <= room.max(1) {
                    piece.push_str(g);
                    used += w;
                } else {
                    rest.push_str(g);
                }
            }
            line.push_str(&piece);
            out.push(std::mem::take(&mut line));
            line = " ".repeat(hang);
            line_w = hang;
            word = rest;
            if word.is_empty() {
                break;
            }
        }
        line.push_str(&word);
        line_w += width(&word);
    }
    if !line.trim().is_empty() {
        out.push(line);
    }
}

/// The failure text of a scenario step without a leading `<step name>: `, which the row or
/// line that shows it already carries.
pub fn without_step_prefix<'a>(name: &str, why: &'a str) -> &'a str {
    why.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(": "))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(why)
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

    /// A copy with every grapheme replaced by `*`, the cursor kept at the same grapheme.
    /// Used to draw secret values.
    pub fn masked(&self) -> TextInput {
        let before = self.text[..self.cursor].graphemes(true).count();
        let total = self.text.graphemes(true).count();
        TextInput {
            text: "*".repeat(total),
            cursor: before,
        }
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
    use super::*;

    #[test]
    fn credentials_are_removed_from_urls() {
        assert_eq!(redact_url("ws://u:p%40w@h:1/x?a=b@c"), "ws://h:1/x?a=b@c");
        assert_eq!(redact_url("wss://tok@h/x"), "wss://h/x");
        assert_eq!(redact_url("ws://h/x@y"), "ws://h/x@y");
        assert_eq!(redact_url("garbage"), "garbage");
    }

    #[test]
    fn wrap_help_never_exceeds_the_width_and_keeps_every_word() {
        let src = [
            "First steps: the station connects and boots at startup (the header shows the state).",
            "     Press : and type a name (BootNotification, Heartbeat) to run it.",
            "",
            "q or Ctrl-C             quit (asks while a scenario or transaction runs, or in a fleet)",
            "Station tab: K config keys, L local auth list, P charging profiles, Y certificates",
            "averyveryverylongwordwithoutanyspacesthatcannotfitinanarrowpopup done",
        ];
        let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join("");
        let expected = words(&src.join(" "));
        for max in [8, 12, 20, 22, 40, 78, 94, 200] {
            let out = wrap_help(&src, max);
            assert!(out.iter().all(|l| width(l) <= max), "max {max}: {out:?}");
            assert_eq!(words(&out.join(" ")), expected, "max {max}");
        }
        // Continuation lines of a key row hang under its description.
        let out = wrap_help(&[src[3]], 50);
        assert!(out.len() > 1);
        assert!(out[1].starts_with(&" ".repeat(24)), "{out:?}");
    }

    #[test]
    fn wrap_text_limits_lines_and_marks_the_cut() {
        let out = wrap_text("alpha beta gamma delta epsilon zeta eta theta", 12, 2);
        assert_eq!(out.len(), 2);
        assert!(out[1].ends_with('…'), "{out:?}");
        assert!(out.iter().all(|l| width(l) <= 12));
        assert_eq!(wrap_text("short", 12, 3), ["short"]);
    }

    #[test]
    fn truncate_middle_keeps_head_and_tail() {
        assert_eq!(truncate_middle("short", 10), "short");
        let out = truncate_middle("LLLLLLLLLLLLLLLLLLLLtail-1", 12);
        assert_eq!(width(&out), 12);
        assert!(
            out.starts_with("LLLLLL") && out.ends_with("ail-1") && out.contains('…'),
            "{out}"
        );
        assert_ne!(
            truncate_middle(&format!("{}A", "L".repeat(40)), 12),
            truncate_middle(&format!("{}B", "L".repeat(40)), 12)
        );
        assert_eq!(truncate_middle("abcdef", 1), "…");
        assert_eq!(truncate_middle("abcdef", 0), "");
    }

    #[test]
    fn masked_input_hides_text_and_keeps_the_cursor() {
        let mut t = TextInput::new("a👍🏽bc");
        t.left();
        let m = t.masked();
        assert_eq!(m.text(), "****");
        assert_eq!(m.window(10), ("****".to_owned(), 3));
        assert_eq!(t.text(), "a👍🏽bc", "the original is untouched");
    }

    #[test]
    fn step_prefix_is_dropped_once() {
        assert_eq!(
            without_step_prefix("authorize", "authorize: Authorize failed"),
            "Authorize failed"
        );
        assert_eq!(without_step_prefix("authorize", "other: x"), "other: x");
        assert_eq!(
            without_step_prefix("authorize", "authorize: "),
            "authorize: "
        );
    }
}
