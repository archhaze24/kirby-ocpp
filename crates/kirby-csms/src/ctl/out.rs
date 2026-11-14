//! What `ctl` prints. Text that came from the API (and through it from stations) is never
//! trusted: characters that could drive a terminal are written as escapes.

use std::fmt::Write as _;
use std::io::Write as _;

use kirby_ocpp_core::text::is_unsafe_char;
use serde_json::Value;

/// `text` (JSON) with every unsafe character above ASCII written as a `\uXXXX` escape, which
/// is still valid JSON. ASCII controls inside strings are already escaped by any JSON
/// producer, and the caller has parsed the text, so none can be left raw in a string; the
/// remaining ones are DEL and the C1, bidirectional and invisible characters.
pub fn escape_json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if u32::from(c) >= 0x7f && is_unsafe_char(c) {
            let _ = write!(out, "\\u{:04x}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether `text` holds a byte below 0x20 outside a JSON string (whitespace between tokens).
fn has_control_outside_string(text: &str) -> bool {
    let (mut in_string, mut escaped) = (false, false);
    for byte in text.bytes() {
        if in_string {
            match (escaped, byte) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte < 0x20 {
            return true;
        }
    }
    false
}

/// The JSON `body` as printed: validated, optionally pretty, escaped. Compact output keeps
/// the bytes the API sent (numbers are not re-rendered) unless they hold a control byte
/// outside a string, such as a bare carriage return, in which case the re-serialized text
/// is printed.
pub fn json_text(body: &[u8], pretty: bool) -> Result<String, String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| "the answer is not JSON; is --url the control API?".to_owned())?;
    let text = if pretty {
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    } else {
        let raw = std::str::from_utf8(body)
            .map_err(|_| "the answer is not UTF-8".to_owned())?
            .trim();
        if has_control_outside_string(raw) {
            serde_json::to_string(&value).map_err(|e| e.to_string())?
        } else {
            raw.to_owned()
        }
    };
    Ok(escape_json_text(&text))
}

/// An error body that is JSON, re-serialized (compact) so that no raw control character
/// outside a string survives, and escaped like every printed JSON text.
pub fn error_json_text(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    Some(escape_json_text(&serde_json::to_string(&value).ok()?))
}

/// Plain text for a terminal: line feeds and tabs stay, other unsafe characters (a bare
/// carriage return too) become `\u{XXXX}`.
pub fn terminal_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_unsafe_char(c) && !matches!(c, '\n' | '\t') {
            let _ = write!(out, "\\u{{{:04x}}}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// Where results go and how they look.
#[derive(Debug, Clone, Copy)]
pub struct Printer {
    pub pretty: bool,
    pub terminal: bool,
}

impl Printer {
    pub fn json(self, body: &[u8]) -> Result<(), String> {
        let mut text = json_text(body, self.pretty)?;
        text.push('\n');
        write_stdout(text.as_bytes())
    }

    /// Text that is not JSON (CSV, NDJSON, metrics): raw bytes to a pipe or file, escaped on
    /// a terminal.
    pub fn text(self, body: &[u8]) -> Result<(), String> {
        if self.terminal {
            let text = terminal_text(&String::from_utf8_lossy(body));
            write_stdout(text.as_bytes())
        } else {
            write_stdout(body)
        }
    }
}

/// The marker of a closed output (a reader that went away), recognized by the caller.
pub const BROKEN_PIPE: &str = "\u{0}broken pipe";

pub fn write_stdout(bytes: &[u8]) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(bytes)
        .and_then(|()| stdout.flush())
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                BROKEN_PIPE.to_owned()
            } else {
                format!("cannot write the output: {error}")
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_json_drops_control_bytes_between_tokens() {
        let raw = "{\"a\":\r1,\"b\":\"x\\\"\\r\",\n\"c\":[2,\t3]}";
        let text = json_text(raw.as_bytes(), false).unwrap();
        assert!(!text.bytes().any(|b| b < 0x20), "{text:?}");
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["a"], 1);
        assert_eq!(back["b"], "x\"\r");
        assert!(text.contains("\"x\\\"\\r\""), "{text}");
        // Clean input keeps its bytes (the number is not re-rendered).
        let clean = json_text(b"{\"n\": 1.50}\n", false).unwrap();
        assert_eq!(clean, "{\"n\": 1.50}");
    }

    #[test]
    fn json_escapes_only_what_is_unsafe_and_stays_json() {
        let raw = "{\"a\":\"x\u{202e}y\u{85}z\u{7f}\",\"b\":\"é я\"}";
        let text = json_text(raw.as_bytes(), false).unwrap();
        assert_eq!(text, "{\"a\":\"x\\u202ey\\u0085z\\u007f\",\"b\":\"é я\"}");
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["a"], "x\u{202e}y\u{85}z\u{7f}");
        let pretty = json_text(raw.as_bytes(), true).unwrap();
        assert!(pretty.contains('\n'), "{pretty}");
        assert!(!pretty.contains('\u{202e}'));
        assert!(serde_json::from_str::<Value>(&pretty).is_ok());
    }

    #[test]
    fn compact_output_keeps_the_numbers_as_sent() {
        let text = json_text(b"{\"n\":1.0,\"big\":18446744073709551615}\n", false).unwrap();
        assert_eq!(text, "{\"n\":1.0,\"big\":18446744073709551615}");
    }

    #[test]
    fn answers_that_are_not_json_are_refused() {
        assert!(json_text(b"<html>", false).is_err());
        assert!(json_text(b"", false).is_err());
        // A raw escape character inside a string is not JSON either.
        assert!(json_text(b"{\"a\":\"\x1b[2J\"}", false).is_err());
    }

    #[test]
    fn terminal_text_keeps_line_ends() {
        let text = terminal_text("a,b\n\x1b[31mred\u{202e}\tz\n");
        assert_eq!(text, "a,b\n\\u{001b}[31mred\\u{202e}\tz\n");
        // A bare carriage return would move the cursor back over the line.
        assert_eq!(terminal_text("a\rb\r\n"), "a\\u{000d}b\\u{000d}\n");
    }

    #[test]
    fn error_text_has_no_raw_control_characters() {
        let text = error_json_text(b"{\"a\":1,\r\"b\":\"x\"}").unwrap();
        assert_eq!(text, "{\"a\":1,\"b\":\"x\"}");
        assert!(error_json_text(b"not json").is_none());
    }
}
