//! Text safety shared by every layer that prints or stores station-controlled strings.

/// Characters that must not reach a terminal, a log line or a file name: C0/C1 controls
/// (escape sequences, newlines), the Unicode line and paragraph separators and the
/// bidirectional overrides.
#[must_use]
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}
