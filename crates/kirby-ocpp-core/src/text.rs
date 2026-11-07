//! Text safety shared by every layer that prints or stores station-controlled strings.

/// Characters that must not reach a terminal, a log line or a file name: C0/C1 controls
/// (escape sequences, newlines), the Unicode line and paragraph separators and the
/// bidirectional overrides, plus the invisible marks (zero-width space and joiners, the
/// left-to-right and right-to-left marks, the Arabic letter mark, the byte order mark).
#[must_use]
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200b}'..='\u{200f}'
                | '\u{061c}'
                | '\u{feff}'
        )
}

/// `text` with every unsafe character written as `\u{XXXX}`, safe to print in a message.
#[must_use]
pub fn escape_unsafe(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_unsafe_char(c) {
            let _ = write!(out, "\\u{{{:04x}}}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{escape_unsafe, is_unsafe_char};

    #[test]
    fn escape_unsafe_writes_code_points() {
        assert_eq!(escape_unsafe("CP\u{7}\u{200b}1"), "CP\\u{0007}\\u{200b}1");
        assert_eq!(escape_unsafe("CP-1"), "CP-1");
    }

    #[test]
    fn controls_separators_and_bidi_are_unsafe() {
        for c in [
            '\0', '\n', '\x1b', '\u{7f}', '\u{85}', '\u{2028}', '\u{2029}',
        ] {
            assert!(is_unsafe_char(c), "{c:?}");
        }
        for c in ('\u{202a}'..='\u{202e}').chain('\u{2066}'..='\u{2069}') {
            assert!(is_unsafe_char(c), "{c:?}");
        }
    }

    #[test]
    fn invisible_marks_are_unsafe() {
        for c in ('\u{200b}'..='\u{200f}').chain(['\u{061c}', '\u{feff}']) {
            assert!(is_unsafe_char(c), "{c:?}");
        }
    }

    #[test]
    fn ordinary_text_is_safe() {
        for c in "CP-001 ä é 充電 ".chars() {
            assert!(!is_unsafe_char(c), "{c:?}");
        }
    }
}
