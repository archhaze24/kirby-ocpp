//! Frame history of a station (cursors, filters, export) and the transaction list.

use std::collections::HashMap;

use axum::extract::Path;
use axum::extract::State;
use axum::extract::rejection::PathRejection;
use axum::http::{HeaderValue, StatusCode, Uri};
use axum::response::Response;
use chrono::{DateTime, Utc};
use kirby_csms_core::{Direction, Frame, FrameEntry, HistoryPage};
use kirby_ocpp_core::text::is_unsafe_char;
use serde_json::{Map, Value, json};

use super::ApiState;
use super::error::{error, reply, unknown_station};
use super::export::{CSV_TYPE, Format, NDJSON_TYPE, csv_table, ndjson, text_response};
use super::json::{query_params, station_id};
use crate::commands;

/// Columns of the CSV history, in this order.
const HISTORY_COLUMNS: &[&str] = &[
    "cursor",
    "ts",
    "station",
    "direction",
    "kind",
    "id",
    "action",
    "payload",
    "code",
    "description",
    "details",
];

/// Columns of the CSV transaction list, in this order.
const TRANSACTION_COLUMNS: &[&str] = &[
    "station",
    "id",
    "connector",
    "id_tag",
    "id_tag_status",
    "meter_start",
    "started_at",
    "status",
    "meter_stop",
    "stopped_at",
    "stop_reason",
    "energy_wh",
    "power_w",
    "last_meter_at",
];

/// Transactions in one answer when `limit` is not given, and the most `limit` may ask for.
const TRANSACTIONS_DEFAULT_LIMIT: usize = 1000;
const TRANSACTIONS_MAX_LIMIT: usize = 10_000;
/// Longest `action` filter, like the fault action names.
const MAX_ACTION_FILTER: usize = 128;

fn bad_request(message: &str) -> Response {
    error(StatusCode::BAD_REQUEST, "bad_request", message)
}

/// A non-negative integer of ASCII digits.
fn parse_count(value: &str) -> Option<usize> {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| value.parse::<usize>().ok())
        .flatten()
}

/// The text of a history cursor: `c` and the sequence number of the frame.
pub(super) fn cursor_text(sequence: u64) -> String {
    format!("c{sequence}")
}

/// The sequence number of a cursor (`c` and up to 20 ASCII digits).
pub(super) fn parse_cursor(text: &str) -> Option<u64> {
    let digits = text.strip_prefix('c')?;
    if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// What a history request asks for.
#[derive(Debug, Default)]
struct HistoryQuery {
    since: Option<u64>,
    limit: Option<usize>,
    action: Option<String>,
    kind: Option<String>,
    direction: Option<String>,
}

/// The frames of an answer and where to continue.
#[derive(Debug, PartialEq)]
struct Selection {
    rows: Vec<Value>,
    next: Option<u64>,
    gap: bool,
}

/// What [`pick`] chose: positions in the held list, and where to continue.
#[derive(Debug, PartialEq)]
struct Pick {
    positions: Vec<usize>,
    next: Option<u64>,
    gap: bool,
}

/// Picks the frames of `held` (frame objects with their sequence numbers, oldest first) that
/// `query` asks for. `dropped_through` is the newest sequence of this station that left the
/// history.
///
/// With `since`, the answer is the first `limit` frames after it; without, the newest `limit`.
/// `next` is the cursor to ask from next: the last frame returned when `limit` cut the answer,
/// else the newest frame held (every frame up to it was looked at, returned or not), else
/// `since`; `None` when there is nothing to point at.
fn pick(held: &[(u64, Value)], dropped_through: u64, query: &HistoryQuery) -> Pick {
    let text = |row: &Value, key: &str| row.get(key).and_then(Value::as_str).map(str::to_owned);
    // A reply names no action: it takes the one of the CALL it answers, which went the other
    // way.
    let mut calls: HashMap<(String, &str), String> = HashMap::new();
    if query.action.is_some() {
        for (_, row) in held {
            if row.get("kind").and_then(Value::as_str) == Some("call")
                && let (Some(id), Some(direction), Some(action)) = (
                    text(row, "id"),
                    row.get("direction").and_then(Value::as_str),
                    text(row, "action"),
                )
            {
                calls.insert((id, if direction == "in" { "in" } else { "out" }), action);
            }
        }
    }
    let wanted = |row: &Value| {
        if query.kind.is_some() && query.kind != text(row, "kind") {
            return false;
        }
        if query.direction.is_some() && query.direction != text(row, "direction") {
            return false;
        }
        if let Some(action) = &query.action {
            let found = if row.get("kind").and_then(Value::as_str) == Some("call") {
                text(row, "action")
            } else {
                let opposite = if row.get("direction").and_then(Value::as_str) == Some("in") {
                    "out"
                } else {
                    "in"
                };
                text(row, "id").and_then(|id| calls.get(&(id, opposite)).cloned())
            };
            return found.as_deref() == Some(action.as_str());
        }
        true
    };
    let after = query.since.unwrap_or(0);
    let limit = query.limit.unwrap_or(usize::MAX);
    let matching: Vec<usize> = held
        .iter()
        .enumerate()
        .filter(|(_, (sequence, row))| *sequence > after && wanted(row))
        .map(|(position, _)| position)
        .collect();
    let newest_held = held
        .last()
        .map(|(sequence, _)| *sequence)
        .filter(|sequence| *sequence > after);
    let (picked, cut): (&[usize], bool) = if query.since.is_some() {
        (
            matching.get(..limit.min(matching.len())).unwrap_or(&[]),
            matching.len() > limit,
        )
    } else {
        (
            matching
                .get(matching.len().saturating_sub(limit)..)
                .unwrap_or(&[]),
            false,
        )
    };
    let sequence_at = |position: usize| held.get(position).map(|(sequence, _)| *sequence);
    let next = if cut {
        picked
            .last()
            .and_then(|position| sequence_at(*position))
            .or(query.since)
    } else {
        newest_held.or(query.since)
    };
    Pick {
        positions: picked.to_vec(),
        next,
        gap: query.since.is_some_and(|since| dropped_through > since),
    }
}

/// The test-facing form of [`pick`]: the picked rows with their `cursor`.
#[cfg(test)]
fn select(held: &[(u64, Value)], dropped_through: u64, query: &HistoryQuery) -> Selection {
    let picked = pick(held, dropped_through, query);
    let rows = picked
        .positions
        .iter()
        .filter_map(|position| held.get(*position))
        .map(|(sequence, row)| {
            let mut row = row.clone();
            if let Some(object) = row.as_object_mut() {
                object.insert("cursor".to_owned(), cursor_text(*sequence).into());
            }
            row
        })
        .collect();
    Selection {
        rows,
        next: picked.next,
        gap: picked.gap,
    }
}

/// The identifying fields of a frame (no payload): what [`pick`] filters on.
fn light_row(entry: &FrameEntry) -> Value {
    let mut row = match &entry.frame {
        Frame::Call { id, action, .. } => {
            json!({"kind": "call", "id": id.clone(), "action": action})
        }
        Frame::CallResult { id, .. } => json!({"kind": "result", "id": id.clone()}),
        Frame::CallError { id, .. } => json!({"kind": "error", "id": id.clone()}),
        other => json!({"kind": "unknown", "id": other.id()}),
    };
    if let Some(object) = row.as_object_mut() {
        object.insert(
            "direction".to_owned(),
            match entry.direction {
                Direction::In => "in",
                Direction::Out => "out",
            }
            .into(),
        );
    }
    row
}

/// Filters on the identifying fields of every held frame; the full JSON (and the clone it
/// needs) is built only for the frames the answer carries.
fn select_page(id: &str, page: &HistoryPage, query: &HistoryQuery) -> Selection {
    let held: Vec<(u64, Value)> = page
        .frames
        .iter()
        .map(|(sequence, entry)| (*sequence, light_row(entry)))
        .collect();
    let picked = pick(&held, page.dropped_through, query);
    let (sequences, entries): (Vec<u64>, Vec<FrameEntry>) = picked
        .positions
        .iter()
        .filter_map(|position| page.frames.get(*position))
        .map(|(sequence, entry)| (*sequence, FrameEntry::clone(entry)))
        .unzip();
    let rows = commands::history_frames(id, &entries)
        .into_iter()
        .zip(sequences)
        .map(|(mut row, sequence)| {
            if let Some(object) = row.as_object_mut() {
                object.insert("cursor".to_owned(), cursor_text(sequence).into());
            }
            row
        })
        .collect();
    Selection {
        rows,
        next: picked.next,
        gap: picked.gap,
    }
}

pub(super) async fn history(
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
    uri: Uri,
) -> Response {
    let id = match station_id(id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let [limit, since, action, kind, direction, format] = match query_params(
        &uri,
        ["limit", "since", "action", "kind", "direction", "format"],
    ) {
        Ok(values) => values,
        Err(message) => return bad_request(&message),
    };
    let mut query = HistoryQuery::default();
    if let Some(limit) = limit {
        let Some(limit) = parse_count(&limit) else {
            return bad_request("`limit` must be a non-negative integer");
        };
        query.limit = Some(limit);
    }
    if let Some(since) = since {
        let Some(cursor) = parse_cursor(&since) else {
            return bad_request("`since` must be a cursor taken from an earlier answer");
        };
        query.since = Some(cursor);
    }
    if let Some(action) = action {
        if action.is_empty()
            || action.len() > MAX_ACTION_FILTER
            || action.chars().any(is_unsafe_char)
        {
            return bad_request("`action` must be 1 to 128 bytes without control characters");
        }
        query.action = Some(action);
    }
    if let Some(kind) = kind {
        if !matches!(kind.as_str(), "call" | "result" | "error") {
            return bad_request("`kind` must be one of: call, result, error");
        }
        query.kind = Some(kind);
    }
    if let Some(direction) = direction {
        if !matches!(direction.as_str(), "in" | "out") {
            return bad_request("`direction` must be one of: in, out");
        }
        query.direction = Some(direction);
    }
    let format = match Format::parse(
        format.as_deref(),
        &[Format::Json, Format::Ndjson, Format::Csv],
    ) {
        Ok(format) => format,
        Err(message) => return bad_request(&message),
    };
    // With an action filter the CALL that a reply answers may be older than the cursor.
    let after = if query.action.is_some() {
        None
    } else {
        query.since
    };
    let Some(page) = state.handle.history_since(&id, after) else {
        return unknown_station();
    };
    if query.since.is_some_and(|since| since > page.newest) {
        return bad_request("`since` is newer than any frame of this run");
    }
    let selection = select_page(&id, &page, &query);
    match format {
        Format::Json => reply(
            StatusCode::OK,
            &json!({
                "station": id,
                "frames": selection.rows,
                "next_cursor": selection.next.map(cursor_text),
                "gap": selection.gap,
            }),
        ),
        Format::Ndjson => text_response(NDJSON_TYPE, ndjson(&selection.rows)),
        Format::Csv => text_response(CSV_TYPE, csv_table(HISTORY_COLUMNS, &selection.rows)),
    }
}

pub(super) async fn transactions(State(state): State<ApiState>, uri: Uri) -> Response {
    let [station, status, since, limit, format] =
        match query_params(&uri, ["station", "state", "since", "limit", "format"]) {
            Ok(values) => values,
            Err(message) => return bad_request(&message),
        };
    if let Some(station) = &station
        && (station.is_empty() || station.len() > 128 || station.chars().any(is_unsafe_char))
    {
        return bad_request("`station` must be 1 to 128 bytes without control characters");
    }
    let active = match status.as_deref() {
        None => None,
        Some("active") => Some(true),
        Some("stopped") => Some(false),
        Some(_) => return bad_request("`state` must be one of: active, stopped"),
    };
    let since = match since {
        None => None,
        Some(text) => match DateTime::parse_from_rfc3339(&text) {
            Ok(time) => Some(time.with_timezone(&Utc)),
            Err(_) => {
                return bad_request(
                    "`since` must be an RFC 3339 timestamp such as 2030-01-01T00:00:00Z",
                );
            }
        },
    };
    let limit = match limit {
        None => TRANSACTIONS_DEFAULT_LIMIT,
        Some(text) => match parse_count(&text) {
            Some(limit) if limit <= TRANSACTIONS_MAX_LIMIT => limit,
            _ => {
                return bad_request("`limit` must be an integer from 0 to 10000");
            }
        },
    };
    let format = match Format::parse(format.as_deref(), &[Format::Json, Format::Csv]) {
        Ok(format) => format,
        Err(message) => return bad_request(&message),
    };
    let (found, truncated) = state
        .handle
        .transactions(station.as_deref(), active, since, limit);
    let rows: Vec<Value> = found
        .into_iter()
        .map(|(station, record)| {
            let mut object = match serde_json::to_value(&record) {
                Ok(Value::Object(object)) => object,
                _ => Map::new(),
            };
            object.insert("station".to_owned(), station.into());
            object.insert("id".to_owned(), record.id.to_string().into());
            Value::Object(object)
        })
        .collect();
    match format {
        Format::Csv => {
            let mut response = text_response(CSV_TYPE, csv_table(TRANSACTION_COLUMNS, &rows));
            response.headers_mut().insert(
                "x-truncated",
                HeaderValue::from_static(if truncated { "true" } else { "false" }),
            );
            response
        }
        _ => reply(
            StatusCode::OK,
            &json!({"transactions": rows, "truncated": truncated}),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, direction: &str, id: &str, action: Option<&str>) -> Value {
        let mut row = json!({"kind": kind, "direction": direction, "id": id});
        if let Some(action) = action {
            row["action"] = action.into();
        }
        row
    }

    /// Frames with sequences 10, 11, ...: a Heartbeat and its answer, a Boot and its answer,
    /// a Reset sent by the server and its answer.
    fn held() -> Vec<(u64, Value)> {
        vec![
            (10, row("call", "in", "1", Some("Heartbeat"))),
            (11, row("result", "out", "1", None)),
            (12, row("call", "in", "2", Some("BootNotification"))),
            (13, row("error", "out", "2", None)),
            (14, row("call", "out", "1", Some("Reset"))),
            (15, row("result", "in", "1", None)),
        ]
    }

    fn cursors(selection: &Selection) -> Vec<&str> {
        selection
            .rows
            .iter()
            .filter_map(|row| row["cursor"].as_str())
            .collect()
    }

    #[test]
    fn cursors_round_trip_and_reject_anything_else() {
        assert_eq!(cursor_text(42), "c42");
        assert_eq!(parse_cursor("c42"), Some(42));
        assert_eq!(parse_cursor("c0"), Some(0));
        for bad in [
            "",
            "c",
            "42",
            "C42",
            "c-1",
            "c+1",
            "c4 2",
            "c4x",
            "c999999999999999999999",
        ] {
            assert_eq!(parse_cursor(bad), None, "{bad}");
        }
        assert_eq!(parse_cursor("c18446744073709551616"), None);
    }

    #[test]
    fn no_filter_returns_everything_or_the_newest() {
        let all = select(&held(), 0, &HistoryQuery::default());
        assert_eq!(cursors(&all), ["c10", "c11", "c12", "c13", "c14", "c15"]);
        assert_eq!(all.next, Some(15));
        assert!(!all.gap);
        let newest = select(
            &held(),
            0,
            &HistoryQuery {
                limit: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(cursors(&newest), ["c14", "c15"]);
        assert_eq!(newest.next, Some(15));
        let none = select(
            &held(),
            0,
            &HistoryQuery {
                limit: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(none.rows.len(), 0);
        assert_eq!(none.next, Some(15));
        let empty = select(&[], 0, &HistoryQuery::default());
        assert_eq!(empty.next, None);
    }

    #[test]
    fn since_pages_forward_without_gaps_or_repeats() {
        let mut query = HistoryQuery {
            limit: Some(4),
            since: Some(0),
            ..Default::default()
        };
        let first = select(&held(), 0, &query);
        assert_eq!(cursors(&first), ["c10", "c11", "c12", "c13"]);
        assert_eq!(first.next, Some(13));
        query.since = first.next;
        let second = select(&held(), 0, &query);
        assert_eq!(cursors(&second), ["c14", "c15"]);
        assert_eq!(second.next, Some(15));
        query.since = second.next;
        let third = select(&held(), 0, &query);
        assert_eq!(third.rows.len(), 0);
        assert_eq!(third.next, Some(15), "an empty page keeps the cursor");
        // A cursor between two held frames.
        query.since = Some(12);
        assert_eq!(cursors(&select(&held(), 0, &query)), ["c13", "c14", "c15"]);
    }

    #[test]
    fn filters_combine_and_replies_follow_their_call() {
        let by = |action: &str| HistoryQuery {
            action: Some(action.to_owned()),
            ..Default::default()
        };
        // The server's Reset (call id 1 going out) and its answer coming in, not the
        // Heartbeat's answer with the same id going out.
        assert_eq!(cursors(&select(&held(), 0, &by("Reset"))), ["c14", "c15"]);
        assert_eq!(
            cursors(&select(&held(), 0, &by("Heartbeat"))),
            ["c10", "c11"]
        );
        assert_eq!(
            cursors(&select(&held(), 0, &by("BootNotification"))),
            ["c12", "c13"]
        );
        assert_eq!(select(&held(), 0, &by("Nope")).rows.len(), 0);
        let kind = HistoryQuery {
            kind: Some("call".into()),
            ..Default::default()
        };
        assert_eq!(cursors(&select(&held(), 0, &kind)), ["c10", "c12", "c14"]);
        let direction = HistoryQuery {
            direction: Some("out".into()),
            ..Default::default()
        };
        assert_eq!(
            cursors(&select(&held(), 0, &direction)),
            ["c11", "c13", "c14"]
        );
        let both = HistoryQuery {
            kind: Some("result".into()),
            direction: Some("in".into()),
            action: Some("Reset".into()),
            ..Default::default()
        };
        assert_eq!(cursors(&select(&held(), 0, &both)), ["c15"]);
    }

    #[test]
    fn a_filtered_poll_always_progresses() {
        // Nothing matches, but every frame was looked at: the cursor moves to the newest.
        let query = HistoryQuery {
            action: Some("Nope".to_owned()),
            since: Some(10),
            ..Default::default()
        };
        let page = select(&held(), 0, &query);
        assert_eq!(page.rows.len(), 0);
        assert_eq!(page.next, Some(15));
        // A cut keeps the cursor on the last frame returned, so the rest follows.
        let query = HistoryQuery {
            kind: Some("call".to_owned()),
            since: Some(0),
            limit: Some(1),
            ..Default::default()
        };
        let page = select(&held(), 0, &query);
        assert_eq!(cursors(&page), ["c10"]);
        assert_eq!(page.next, Some(10));
    }

    #[test]
    fn a_gap_is_reported_when_frames_after_the_cursor_were_dropped() {
        let query = HistoryQuery {
            since: Some(8),
            ..Default::default()
        };
        assert!(select(&held(), 9, &query).gap);
        assert!(!select(&held(), 8, &query).gap);
        assert!(!select(&held(), 9, &HistoryQuery::default()).gap);
    }
}
