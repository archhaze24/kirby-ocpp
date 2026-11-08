//! Export formats: CSV (RFC 4180) and NDJSON responses, and the text helpers they share.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use kirby_ocpp_core::text::is_unsafe_char;
use kirby_ocpp_tui::text::escape_json_unsafe;
use serde_json::Value;

/// The value of the `format` query parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Json,
    Ndjson,
    Csv,
}

impl Format {
    /// Parses `value` when it is one of `allowed`.
    pub(super) fn parse(value: Option<&str>, allowed: &[Self]) -> Result<Self, String> {
        let Some(value) = value else {
            return Ok(Self::Json);
        };
        let format = match value {
            "json" => Some(Self::Json),
            "ndjson" => Some(Self::Ndjson),
            "csv" => Some(Self::Csv),
            _ => None,
        };
        if let Some(format) = format
            && allowed.contains(&format)
        {
            Ok(format)
        } else {
            let names: Vec<&str> = allowed
                .iter()
                .map(|format| match format {
                    Self::Json => "json",
                    Self::Ndjson => "ndjson",
                    Self::Csv => "csv",
                })
                .collect();
            Err(format!("`format` must be one of: {}", names.join(", ")))
        }
    }
}

/// One CSV cell: text from stations is attacker controlled, so control and bidirectional
/// characters become U+FFFD, a cell that a spreadsheet would read as a formula (it starts with
/// `=`, `+`, `-` or `@`) gets a leading `'`, and a cell with a comma or a quote is quoted.
pub(super) fn csv_text(text: &str) -> String {
    let mut clean: String = text
        .chars()
        .map(|c| if is_unsafe_char(c) { '\u{fffd}' } else { c })
        .collect();
    if clean.starts_with(['=', '+', '-', '@']) {
        clean.insert(0, '\'');
    }
    quote(&clean)
}

/// Quotes `text` when RFC 4180 needs it.
fn quote(text: &str) -> String {
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

/// A CSV cell for a JSON value: `null` and a missing value are empty, a number is written as
/// is (never prefixed: it is not text), a string through [`csv_text`], anything else as
/// compact JSON text through [`csv_text`].
pub(super) fn csv_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::String(text)) => csv_text(text),
        Some(other) => csv_text(&other.to_string()),
    }
}

/// A CSV document: the header row, then one row per object, each cell the member of the
/// object named by the column. Lines end in CRLF.
pub(super) fn csv_table(columns: &[&str], rows: &[Value]) -> String {
    let mut out = columns.join(",");
    out.push_str("\r\n");
    for row in rows {
        let cells: Vec<String> = columns
            .iter()
            .map(|column| csv_value(row.get(*column)))
            .collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

/// One JSON object per line, escaped like every other JSON body of the API.
pub(super) fn ndjson(rows: &[Value]) -> String {
    let mut out = String::new();
    for row in rows {
        out.push_str(&escape_json_unsafe(&row.to_string()));
        out.push('\n');
    }
    out
}

/// A `200` response with `body` as `content_type`.
pub(super) fn text_response(content_type: &'static str, body: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        body,
    )
        .into_response()
}

pub(super) const CSV_TYPE: &str = "text/csv; charset=utf-8";
pub(super) const NDJSON_TYPE: &str = "application/x-ndjson";

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn cells_are_quoted_and_formula_safe() {
        assert_eq!(csv_text("plain"), "plain");
        assert_eq!(csv_text("a,b"), "\"a,b\"");
        assert_eq!(csv_text("say \"hi\""), "\"say \"\"hi\"\"\"");
        for lead in ['=', '+', '-', '@'] {
            assert_eq!(csv_text(&format!("{lead}cmd")), format!("'{lead}cmd"));
        }
        assert_eq!(csv_text("=a,b"), "\"'=a,b\"");
        assert_eq!(csv_text("a=b"), "a=b");
        assert_eq!(csv_text(""), "");
    }

    #[test]
    fn unsafe_characters_never_reach_a_cell() {
        let text = csv_text("a\r\nb\u{1b}[31m\u{202e}c\td");
        assert!(!text.chars().any(is_unsafe_char), "{text:?}");
        assert!(text.contains('\u{fffd}'));
        // A leading tab or CR cannot be used to dodge the formula prefix.
        assert_eq!(csv_text("\t=1"), "\u{fffd}=1");
    }

    #[test]
    fn values_follow_their_json_type() {
        assert_eq!(csv_value(None), "");
        assert_eq!(csv_value(Some(&Value::Null)), "");
        assert_eq!(csv_value(Some(&json!(-5))), "-5");
        assert_eq!(csv_value(Some(&json!(1.5))), "1.5");
        assert_eq!(csv_value(Some(&json!(true))), "true");
        assert_eq!(csv_value(Some(&json!("=1+1"))), "'=1+1");
        assert_eq!(
            csv_value(Some(&json!({"a": [1, "x,y"]}))),
            "\"{\"\"a\"\":[1,\"\"x,y\"\"]}\""
        );
        assert_eq!(csv_value(Some(&json!("-7"))), "'-7");
    }

    #[test]
    fn a_table_has_a_header_and_crlf_rows() {
        let rows = [json!({"a": 1, "b": "x"}), json!({"b": "=y"})];
        assert_eq!(
            csv_table(&["a", "b", "c"], &rows),
            "a,b,c\r\n1,x,\r\n,'=y,\r\n"
        );
        assert_eq!(csv_table(&["a"], &[]), "a\r\n");
    }

    #[test]
    fn ndjson_is_one_escaped_object_per_line() {
        let text = ndjson(&[json!({"a": "x\u{202e}y"}), json!({"b": 2})]);
        assert_eq!(text.lines().count(), 2);
        assert!(!text.contains('\u{202e}'), "{text:?}");
        assert!(text.ends_with('\n'));
        assert_eq!(ndjson(&[]), "");
    }

    #[test]
    fn formats_are_checked_against_the_allowed_set() {
        let all = [Format::Json, Format::Ndjson, Format::Csv];
        assert_eq!(Format::parse(None, &all), Ok(Format::Json));
        assert_eq!(Format::parse(Some("csv"), &all), Ok(Format::Csv));
        assert_eq!(Format::parse(Some("ndjson"), &all), Ok(Format::Ndjson));
        assert!(Format::parse(Some("xml"), &all).is_err());
        assert!(Format::parse(Some(""), &all).is_err());
        assert!(Format::parse(Some("CSV"), &all).is_err());
        assert!(Format::parse(Some("ndjson"), &[Format::Json, Format::Csv]).is_err());
    }
}
