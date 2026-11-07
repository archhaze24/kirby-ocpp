//! The log: a bounded ring buffer of frames and station lines with CALL/RESULT
//! correlation, filters, search and a selection that follows the tail or stays put.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use kirby_ocpp_core::station::{Direction, Event, FrameRecord, LogLevel, LogRecord};
use serde_json::Value;

use crate::text::sanitize;

pub const LOG_CAPACITY: usize = 2000;

/// Approximate bytes one log may hold; the oldest entries go first.
pub const LOG_BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// Most frame (or station line) text kept per entry. A larger frame is stored as its first
/// `ENTRY_TEXT_MAX` bytes of compact JSON plus a `…[truncated N bytes]` marker.
pub const ENTRY_TEXT_MAX: usize = 16 * 1024;

/// Longest row summary kept.
const SUMMARY_MAX: usize = 1024;

/// How much of an entry's text the search looks at.
const HAYSTACK_TEXT_MAX: usize = 4 * 1024;

/// Which entries are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFilter {
    All,
    /// OCPP frames only (CALL, CALLRESULT, CALLERROR).
    Calls,
    /// CALLERRORs and warning/error station lines.
    Errors,
    /// Station log lines only.
    Station,
    /// Frames received from the CSMS.
    Csms,
}

impl LogFilter {
    pub const ALL: [Self; 5] = [
        Self::All,
        Self::Calls,
        Self::Errors,
        Self::Station,
        Self::Csms,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Calls => "calls",
            Self::Errors => "errors",
            Self::Station => "station",
            Self::Csms => "csms",
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// OCPP-J message kind of a frame row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Call,
    Result,
    Error,
    /// Not a valid frame (raw text).
    Raw,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    /// Monotonic sequence number, never reused.
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub event: Event,
    pub kind: Option<FrameKind>,
    pub summary: String,
    /// Round-trip time, set on the answering row.
    pub rtt_ms: Option<i64>,
    /// Sequence number of the CALL (on answers) or of the answer (on CALLs).
    pub peer: Option<u64>,
    /// Bytes cut from the frame or line to keep it small (`None`: stored whole).
    pub truncated: Option<usize>,
    /// Lower-cased text the search matches against.
    haystack: String,
    /// Approximate memory held by the entry.
    bytes: usize,
}

impl LogEntry {
    pub fn frame(&self) -> Option<&FrameRecord> {
        match &self.event {
            Event::Frame(f) => Some(f),
            _ => None,
        }
    }

    pub fn station_line(&self) -> Option<&LogRecord> {
        match &self.event {
            Event::Log(l) => Some(l),
            _ => None,
        }
    }

    /// The frame as one line of JSON (what "copy visible" uses); truncated frames end in the
    /// marker.
    pub fn compact(&self) -> String {
        match &self.event {
            Event::Frame(f) => self.compact_text(f),
            _ => self.pretty(),
        }
    }

    fn compact_text(&self, f: &FrameRecord) -> String {
        match (&f.frame, self.truncated) {
            (Value::String(kept), Some(_)) => kept.clone(),
            _ => f.frame.to_string(),
        }
    }

    fn is_error(&self) -> bool {
        self.kind == Some(FrameKind::Error)
            || self
                .station_line()
                .is_some_and(|l| l.level == LogLevel::Error)
    }

    fn matches(&self, filter: LogFilter) -> bool {
        match filter {
            LogFilter::All => true,
            LogFilter::Calls => self.kind.is_some(),
            LogFilter::Errors => self.is_error(),
            LogFilter::Station => self.station_line().is_some(),
            LogFilter::Csms => self.frame().is_some_and(|f| f.direction == Direction::In),
        }
    }

    /// The entry as pretty text (what "copy" and the detail view show).
    ///
    /// A truncated entry yields its kept text and the truncation marker.
    pub fn pretty(&self) -> String {
        match &self.event {
            Event::Frame(f) if self.truncated.is_some() => self.compact_text(f),
            Event::Frame(f) => serde_json::to_string_pretty(&f.frame).unwrap_or_default(),
            Event::Log(l) => l.text.clone(),
            Event::StateChanged => String::new(),
        }
    }
}

/// The longest prefix of `text` of at most `max` bytes that ends on a character boundary.
fn prefix(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[derive(Debug)]
pub struct LogStore {
    entries: VecDeque<LogEntry>,
    capacity: usize,
    max_bytes: usize,
    bytes: usize,
    next_seq: u64,
    /// CALL message id -> (seq, ts) while unanswered.
    pending: HashMap<String, (u64, DateTime<Utc>)>,
    pub filter: LogFilter,
    pub search: String,
    /// `true`: the selection follows the newest visible entry.
    pub follow: bool,
    selected: Option<u64>,
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new(LOG_CAPACITY)
    }
}

impl LogStore {
    /// Most rows kept.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: capacity.max(1),
            max_bytes: LOG_BYTE_BUDGET,
            bytes: 0,
            next_seq: 0,
            pending: HashMap::new(),
            filter: LogFilter::All,
            search: String::new(),
            follow: true,
            selected: None,
        }
    }

    /// Sets the approximate memory budget (default [`LOG_BYTE_BUDGET`]).
    #[must_use]
    pub fn with_byte_budget(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes.max(1);
        self
    }

    /// Approximate memory held by the entries.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.pending.clear();
        self.selected = None;
        self.follow = true;
    }

    pub fn push(&mut self, event: Event) {
        let Some(entry) = self.build(event) else {
            return;
        };
        self.bytes += entry.bytes;
        self.entries.push_back(entry);
        // Evict the oldest while over the count or the byte budget (the newest always stays).
        while self.entries.len() > 1
            && (self.entries.len() > self.capacity || self.bytes > self.max_bytes)
        {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.bytes);
            if old.kind == Some(FrameKind::Call)
                && let Some(id) = old.frame().and_then(|f| f.msg_id.as_ref())
                && self.pending.get(id).is_some_and(|(s, _)| *s == old.seq)
            {
                self.pending.remove(id);
            }
            if self.selected == Some(old.seq) {
                self.selected = self.entries.front().map(|e| e.seq);
            }
        }
    }

    fn build(&mut self, mut event: Event) -> Option<LogEntry> {
        let seq = self.next_seq;
        let mut truncated = None;
        // A parsed `Value` costs a few times its JSON text.
        let held: usize;
        let (ts, kind, summary, haystack) = match &mut event {
            Event::Frame(f) => {
                let kind = frame_kind(&f.frame);
                // The row shows a short summary; a huge string field must not be kept whole.
                let summary = prefix(&summarize(&f.frame), SUMMARY_MAX).to_owned();
                let text = f.frame.to_string();
                if text.len() > ENTRY_TEXT_MAX {
                    let kept = prefix(&text, ENTRY_TEXT_MAX);
                    let cut = text.len() - kept.len();
                    f.frame = Value::String(format!("{kept}…[truncated {cut} bytes]"));
                    truncated = Some(cut);
                }
                held = match &f.frame {
                    Value::String(kept) if truncated.is_some() => kept.len(),
                    _ => text.len().saturating_mul(3),
                };
                let hay = format!(
                    "{} {} {}",
                    f.action.as_deref().unwrap_or(""),
                    summary,
                    prefix(&text, HAYSTACK_TEXT_MAX)
                )
                .to_lowercase();
                (f.ts, Some(kind), summary, hay)
            }
            Event::Log(l) => {
                if l.text.len() > ENTRY_TEXT_MAX {
                    let cut = l.text.len() - prefix(&l.text, ENTRY_TEXT_MAX).len();
                    l.text.truncate(l.text.len() - cut);
                    let marker = format!("…[truncated {cut} bytes]");
                    l.text.push_str(&marker);
                    truncated = Some(cut);
                }
                held = l.text.len();
                let text = sanitize(&l.text);
                (l.ts, None, text.clone(), text.to_lowercase())
            }
            Event::StateChanged => return None,
        };
        self.next_seq += 1;
        let mut rtt_ms = None;
        let mut peer = None;
        if let Event::Frame(f) = &event
            && let Some(id) = &f.msg_id
        {
            match kind {
                Some(FrameKind::Call) => {
                    self.pending.insert(id.clone(), (seq, ts));
                }
                Some(FrameKind::Result | FrameKind::Error) => {
                    if let Some((call_seq, call_ts)) = self.pending.remove(id) {
                        rtt_ms = Some((ts - call_ts).num_milliseconds().max(0));
                        peer = Some(call_seq);
                        if let Some(call) = self.get_mut(call_seq) {
                            call.peer = Some(seq);
                        }
                    }
                }
                _ => {}
            }
        }
        let bytes = held + summary.len() + haystack.len() + 256;
        Some(LogEntry {
            seq,
            ts,
            event,
            kind,
            summary,
            rtt_ms,
            peer,
            truncated,
            haystack,
            bytes,
        })
    }

    fn index_of(&self, seq: u64) -> Option<usize> {
        let first = self.entries.front()?.seq;
        let i = usize::try_from(seq.checked_sub(first)?).ok()?;
        (i < self.entries.len()).then_some(i)
    }

    fn get_mut(&mut self, seq: u64) -> Option<&mut LogEntry> {
        let i = self.index_of(seq)?;
        self.entries.get_mut(i)
    }

    pub fn get(&self, seq: u64) -> Option<&LogEntry> {
        self.entries.get(self.index_of(seq)?)
    }

    fn passes(&self, e: &LogEntry) -> bool {
        e.matches(self.filter)
            && (self.search.is_empty() || e.haystack.contains(&self.search.to_lowercase()))
    }

    /// Entries that pass the filter and search, oldest first.
    pub fn visible(&self) -> Vec<&LogEntry> {
        let needle = self.search.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                e.matches(self.filter) && (needle.is_empty() || e.haystack.contains(&needle))
            })
            .collect()
    }

    pub fn visible_count(&self) -> usize {
        self.entries.iter().filter(|e| self.passes(e)).count()
    }

    /// Index of the selected entry within [`Self::visible`] (the newest while following).
    pub fn selected_index(&self) -> Option<usize> {
        let vis = self.visible();
        if vis.is_empty() {
            return None;
        }
        if self.follow {
            return Some(vis.len() - 1);
        }
        match self.selected {
            Some(seq) => Some(
                vis.iter()
                    .position(|e| e.seq >= seq)
                    .unwrap_or(vis.len() - 1),
            ),
            None => Some(vis.len() - 1),
        }
    }

    pub fn selected(&self) -> Option<&LogEntry> {
        let vis = self.visible();
        self.selected_index().map(|i| vis[i])
    }

    /// Moves the selection by `delta` rows; leaves follow mode unless it lands on the last row.
    pub fn move_selection(&mut self, delta: isize) {
        let vis = self.visible();
        let Some(cur) = self.selected_index() else {
            return;
        };
        let max = vis.len() - 1;
        let next = cur.saturating_add_signed(delta).min(max);
        self.selected = Some(vis[next].seq);
        self.follow = next == max;
    }

    pub fn select_first(&mut self) {
        if let Some(e) = self.visible().first() {
            self.selected = Some(e.seq);
            self.follow = false;
        }
    }

    pub fn follow_tail(&mut self) {
        self.follow = true;
    }

    pub fn toggle_follow(&mut self) {
        if self.follow {
            self.selected = self.selected().map(|e| e.seq);
            self.follow = false;
        } else {
            self.follow = true;
        }
    }

    pub fn set_filter(&mut self, filter: LogFilter) {
        let keep = self.selected().map(|e| e.seq);
        self.filter = filter;
        self.selected = keep;
    }

    /// All entries, oldest first (for export).
    pub fn all(&self) -> impl Iterator<Item = &LogEntry> {
        self.entries.iter()
    }
}

fn frame_kind(frame: &Value) -> FrameKind {
    match frame.get(0).and_then(Value::as_u64) {
        Some(2) => FrameKind::Call,
        Some(3) => FrameKind::Result,
        Some(4) => FrameKind::Error,
        _ => FrameKind::Raw,
    }
}

/// A short one-line description of a frame's payload.
pub fn summarize(frame: &Value) -> String {
    let Some(arr) = frame.as_array() else {
        return sanitize(&frame.to_string());
    };
    match frame_kind(frame) {
        FrameKind::Call => arr.get(3).map(scalar_fields).unwrap_or_default(),
        FrameKind::Result => arr.get(2).map(scalar_fields).unwrap_or_default(),
        FrameKind::Error => {
            let code = arr.get(2).and_then(Value::as_str).unwrap_or("?");
            let desc = arr.get(3).and_then(Value::as_str).unwrap_or("");
            sanitize(format!("{code} {desc}").trim())
        }
        FrameKind::Raw => sanitize(&frame.to_string()),
    }
}

fn scalar_fields(payload: &Value) -> String {
    let Some(map) = payload.as_object() else {
        return sanitize(&payload.to_string());
    };
    // `status` first: it is what one looks for in a response.
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_by_key(|k| k.as_str() != "status");
    let parts: Vec<String> = keys
        .into_iter()
        .filter_map(|k| {
            let v = &map[k];
            let shown = match v {
                Value::String(s) => s.clone(),
                Value::Number(_) | Value::Bool(_) => v.to_string(),
                _ => return None,
            };
            Some(sanitize(&format!("{k}={shown}")))
        })
        .take(4)
        .collect();
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    fn at(ms: i64) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_700_000_000_000 + ms).unwrap()
    }

    fn frame(ms: i64, dir: Direction, id: &str, action: Option<&str>, frame: Value) -> Event {
        Event::Frame(FrameRecord {
            ts: at(ms),
            direction: dir,
            msg_id: Some(id.to_owned()),
            action: action.map(str::to_owned),
            frame,
        })
    }

    fn line(ms: i64, level: LogLevel, text: &str) -> Event {
        Event::Log(LogRecord {
            ts: at(ms),
            level,
            text: text.to_owned(),
        })
    }

    fn call(ms: i64, id: &str) -> Event {
        frame(
            ms,
            Direction::Out,
            id,
            Some("Authorize"),
            json!([2, id, "Authorize", {"idTag": "ABC"}]),
        )
    }

    #[test]
    fn ring_buffer_drops_oldest_and_keeps_sequence_lookup() {
        let mut log = LogStore::new(3);
        for i in 0..5 {
            log.push(line(i, LogLevel::Info, &format!("l{i}")));
        }
        assert_eq!(log.len(), 3);
        let texts: Vec<_> = log.visible().iter().map(|e| e.summary.clone()).collect();
        assert_eq!(texts, ["l2", "l3", "l4"]);
        assert!(log.get(1).is_none());
        assert_eq!(log.get(3).unwrap().summary, "l3");
    }

    #[test]
    fn result_is_correlated_with_its_call_and_carries_the_round_trip() {
        let mut log = LogStore::default();
        log.push(call(0, "m1"));
        log.push(frame(
            42,
            Direction::In,
            "m1",
            Some("Authorize"),
            json!([3, "m1", {"idTagInfo": {"status": "Accepted"}, "status": "Accepted"}]),
        ));
        let vis = log.visible();
        assert_eq!(vis[1].rtt_ms, Some(42));
        assert_eq!(vis[1].peer, Some(0));
        assert_eq!(vis[0].peer, Some(1));
        assert_eq!(vis[0].summary, "idTag=ABC");
        assert_eq!(vis[1].summary, "status=Accepted");
    }

    #[test]
    fn unanswered_call_has_no_rtt_and_unknown_result_is_ignored() {
        let mut log = LogStore::default();
        log.push(frame(0, Direction::In, "x", None, json!([3, "x", {}])));
        assert_eq!(log.visible()[0].rtt_ms, None);
    }

    #[test]
    fn errors_filter_leaves_out_warnings_and_info() {
        let mut log = LogStore::default();
        log.push(line(0, LogLevel::Warn, "security event StartupOfTheDevice"));
        log.push(line(1, LogLevel::Info, "hello"));
        log.push(line(2, LogLevel::Error, "boom"));
        log.set_filter(LogFilter::Errors);
        assert_eq!(log.visible_count(), 1);
    }

    #[test]
    fn filters_select_the_right_rows() {
        let mut log = LogStore::default();
        log.push(call(0, "a"));
        log.push(frame(
            1,
            Direction::In,
            "a",
            Some("Authorize"),
            json!([3, "a", {}]),
        ));
        log.push(frame(
            2,
            Direction::In,
            "b",
            None,
            json!([4, "b", "NotSupported", "nope", {}]),
        ));
        log.push(line(3, LogLevel::Info, "hello"));
        log.push(line(4, LogLevel::Error, "boom"));
        let count = |log: &mut LogStore, f| {
            log.set_filter(f);
            log.visible_count()
        };
        assert_eq!(count(&mut log, LogFilter::All), 5);
        assert_eq!(count(&mut log, LogFilter::Calls), 3);
        assert_eq!(count(&mut log, LogFilter::Errors), 2);
        assert_eq!(count(&mut log, LogFilter::Station), 2);
        assert_eq!(count(&mut log, LogFilter::Csms), 2);
    }

    #[test]
    fn search_matches_action_summary_and_payload_case_insensitively() {
        let mut log = LogStore::default();
        log.push(call(0, "a"));
        log.push(line(1, LogLevel::Info, "Connected"));
        log.search = "AUTHORIZE".into();
        assert_eq!(log.visible_count(), 1);
        log.search = "abc".into();
        assert_eq!(log.visible_count(), 1);
        log.search = "connected".into();
        assert_eq!(log.visible_count(), 1);
        log.search = "nothing".into();
        assert!(log.selected().is_none());
    }

    #[test]
    fn selection_follows_until_moved_then_stays() {
        let mut log = LogStore::default();
        for i in 0..4 {
            log.push(line(i, LogLevel::Info, &format!("l{i}")));
        }
        assert_eq!(log.selected().unwrap().summary, "l3");
        log.move_selection(-2);
        assert!(!log.follow);
        assert_eq!(log.selected().unwrap().summary, "l1");
        log.push(line(9, LogLevel::Info, "l4"));
        assert_eq!(log.selected().unwrap().summary, "l1");
        log.move_selection(10);
        assert!(log.follow);
        assert_eq!(log.selected().unwrap().summary, "l4");
        log.toggle_follow();
        log.push(line(10, LogLevel::Info, "l5"));
        assert_eq!(log.selected().unwrap().summary, "l4");
    }

    #[test]
    fn payload_text_cannot_inject_control_sequences() {
        let mut log = LogStore::default();
        log.push(line(0, LogLevel::Info, "a\x1b[2Jb"));
        assert!(!log.visible()[0].summary.contains('\x1b'));
        log.push(frame(
            1,
            Direction::Out,
            "i",
            Some("X"),
            json!([2, "i", "X", {"k": "\u{1b}[31m"}]),
        ));
        assert!(!log.visible()[1].summary.contains('\x1b'));
    }

    fn big_call(ms: i64, id: &str, numbers: usize) -> Event {
        let data: Vec<u32> = (0..u32::try_from(numbers).unwrap()).collect();
        frame(
            ms,
            Direction::Out,
            id,
            Some("DataTransfer"),
            json!([2, id, "DataTransfer", {"data": data, "tail": ["needle-at-the-end"]}]),
        )
    }

    #[test]
    fn large_frames_are_truncated_with_a_marker() {
        let mut log = LogStore::default();
        log.push(big_call(0, "1", 100_000));
        let e = log.get(0).unwrap();
        let cut = e.truncated.expect("truncated");
        assert!(cut > 100_000);
        assert!(e.bytes < ENTRY_TEXT_MAX * 2 + 4096, "{}", e.bytes);
        let text = e.pretty();
        assert!(
            text.starts_with("[2,\"1\",\"DataTransfer\""),
            "{}",
            &text[..40]
        );
        assert!(text.ends_with(&format!("…[truncated {cut} bytes]")));
        assert_eq!(e.compact(), text);
        // Search looks at the head only; the summary still works.
        log.search = "needle-at-the-end".to_owned();
        assert_eq!(log.visible_count(), 0);
        log.search = "datatransfer".to_owned();
        assert_eq!(log.visible_count(), 1);
    }

    #[test]
    fn small_frames_are_stored_whole() {
        let mut log = LogStore::default();
        log.push(big_call(0, "1", 10));
        let e = log.get(0).unwrap();
        assert!(e.truncated.is_none());
        assert!(e.pretty().contains("needle-at-the-end"));
    }

    #[test]
    fn long_station_lines_are_truncated_on_a_char_boundary() {
        let mut log = LogStore::default();
        log.push(Event::Log(LogRecord {
            ts: at(0),
            level: LogLevel::Info,
            text: "é".repeat(ENTRY_TEXT_MAX),
        }));
        let e = log.get(0).unwrap();
        assert!(e.truncated.is_some());
        assert!(e.pretty().contains("…[truncated"));
    }

    #[test]
    fn byte_budget_evicts_the_oldest() {
        let mut log = LogStore::default().with_byte_budget(200_000);
        for i in 0..40 {
            log.push(big_call(i, &i.to_string(), 20_000));
        }
        assert!(log.len() < 40 && !log.is_empty());
        assert!(
            log.bytes() <= 200_000 + ENTRY_TEXT_MAX * 3,
            "{}",
            log.bytes()
        );
        // The newest entry is kept and the bookkeeping matches the entries.
        assert_eq!(log.get(39).unwrap().seq, 39);
        let sum: usize = log.entries.iter().map(|e| e.bytes).sum();
        assert_eq!(sum, log.bytes());
        // Even an over-budget single entry stays.
        let mut tiny = LogStore::default().with_byte_budget(1);
        tiny.push(big_call(0, "1", 10));
        assert_eq!(tiny.len(), 1);
    }
}
