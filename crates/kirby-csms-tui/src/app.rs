//! The pure model: [`update`] turns a [`Msg`] into [`Effect`]s, nothing here touches the
//! terminal or the engine. The run loop executes the effects and feeds their results back.

use std::collections::{BTreeSet, HashMap, VecDeque};

use chrono::{DateTime, Utc};
use kirby_csms_core::Direction as CsmsDir;
use kirby_csms_core::{
    CsmsEvent, DisconnectReason, Frame, FrameEntry, StationInfo, StationState, TransactionStatus,
};
use kirby_ocpp_core::station::{Direction, Event, FrameRecord, LogLevel, LogRecord};
use kirby_ocpp_tui::app::{Detail, NoticeLevel};
use kirby_ocpp_tui::logview::{LOG_CAPACITY, LogEntry, LogFilter, LogStore};
use kirby_ocpp_tui::text::TextInput;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::actions::{Palette, ResultRecord, ResultsList, Rules};
use crate::command::{CmdOutcome, Command};
use crate::form::{FaultKind, Form};
use crate::keys;

/// Redraw ticks a notice stays visible (about three seconds at 30 fps).
const NOTICE_TICKS: u32 = 100;
/// Ticks during which a frame equal to one of the history tail is dropped as a duplicate.
const DEDUPE_TICKS: u32 = 10;
/// History entries remembered for the duplicate check.
const DEDUPE_ENTRIES: usize = 16;
/// Most unanswered CALLs the correlation remembers per view.
const TRACKER_CAP: usize = 4096;
/// Separates the station id from the message id in the all-stations log, where message ids
/// of different stations can collide.
const KEY_SEP: char = '\u{1f}';

/// Which pane the navigation keys act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Stations,
    Log,
}

/// Which stream the log pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogView {
    /// The selected station only.
    Station,
    /// The merged live stream of every station.
    All,
}

/// What a text input edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTarget {
    /// The station list filter (fuzzy, by id).
    Stations,
    /// The log search.
    LogSearch,
}

/// A one-line text input being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub target: InputTarget,
    pub text: TextInput,
}

/// What a confirmed dialog does.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    Send { station: String, command: Command },
    Forget(String),
    ClearRules,
}

/// A yes/no question; `y` confirms, any other key cancels.
#[derive(Debug, Clone, PartialEq)]
pub struct Confirm {
    pub message: String,
    pub action: ConfirmAction,
}

/// Exactly one mode owns the keyboard at a time.
#[derive(Debug)]
pub enum Mode {
    Normal,
    Input(Input),
    Help,
    Detail(Detail),
    Palette(Palette),
    Form(Box<Form>),
    Confirm(Confirm),
    Results(ResultsList),
}

/// A transient message for the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub level: NoticeLevel,
    ttl: u32,
}

/// What the list needs to know about a station; updated on `StateChanged`, never the full
/// [`StationState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub id: String,
    pub connected: bool,
    /// The last connection was taken over by a newer one with the same id.
    pub replaced: bool,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub active_tx: usize,
}

impl Summary {
    fn new(id: String, connected: bool) -> Self {
        Self {
            id,
            connected,
            replaced: false,
            vendor: None,
            model: None,
            active_tx: 0,
        }
    }

    fn apply(&mut self, state: &StationState) {
        self.vendor = state.boot.as_ref().map(|b| b.vendor.clone());
        self.model = state.boot.as_ref().map(|b| b.model.clone());
        self.active_tx = state
            .transactions
            .iter()
            .filter(|t| t.status == TransactionStatus::Active)
            .count();
    }
}

/// Remembers the action of unanswered CALLs so their results can be searched by action.
#[derive(Debug, Default)]
struct ActionTracker {
    pending: HashMap<String, String>,
}

impl ActionTracker {
    /// Builds the log record for a frame; `key` is the (station-qualified) message id.
    fn record(
        &mut self,
        ts: DateTime<Utc>,
        direction: Direction,
        key: String,
        frame: &Frame,
    ) -> FrameRecord {
        let action = if let Some(action) = frame.action() {
            if self.pending.len() >= TRACKER_CAP {
                self.pending.clear();
            }
            self.pending.insert(key.clone(), action.to_owned());
            Some(action.to_owned())
        } else {
            self.pending.remove(&key)
        };
        FrameRecord {
            ts,
            direction,
            msg_id: Some(key),
            action,
            frame: frame.to_value(),
        }
    }
}

/// Station tags of the all-stations log, aligned with the entries of its [`LogStore`].
#[derive(Debug, Default)]
struct Tags {
    tags: VecDeque<String>,
    pushed: u64,
}

impl Tags {
    fn push(&mut self, capacity: usize, tag: String) {
        self.tags.push_back(tag);
        self.pushed += 1;
        while self.tags.len() > capacity {
            self.tags.pop_front();
        }
    }

    fn get(&self, seq: u64) -> Option<&str> {
        let first = self
            .pushed
            .checked_sub(u64::try_from(self.tags.len()).ok()?)?;
        let index = usize::try_from(seq.checked_sub(first)?).ok()?;
        self.tags.get(index).map(String::as_str)
    }
}

/// Everything the UI shows and edits.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent redraw / quit / stop / refresh flags"
)]
pub struct App {
    /// The URL stations connect to.
    pub url: String,
    /// Every station of the registry, ordered by id.
    pub stations: Vec<Summary>,
    /// The list filter text (fuzzy, by id); empty means no filter.
    pub station_filter: String,
    /// Indices into `stations` that match the filter, valid only while it is not empty.
    matches: Vec<usize>,
    /// Id of the selected station.
    pub selected: Option<String>,
    /// Registry info of the selected station.
    pub info: Option<StationInfo>,
    /// Protocol state of the selected station.
    pub state: Option<StationState>,
    pub focus: Focus,
    pub mode: Mode,
    pub view: LogView,
    /// Frames of the selected station.
    pub station_log: LogStore,
    /// The merged stream of all stations.
    pub all_log: LogStore,
    tags: Tags,
    station_tracker: ActionTracker,
    all_tracker: ActionTracker,
    dedupe: Vec<FrameEntry>,
    dedupe_ticks: u32,
    /// Frames received from / sent to stations since the UI started.
    pub frames_in: u64,
    pub frames_out: u64,
    pub notice: Option<Notice>,
    pub now: DateTime<Utc>,
    pub size: (u16, u16),
    pub quit: bool,
    pub dirty: bool,
    /// The engine's event channel closed (the server was shut down).
    pub stopped: bool,
    dirty_states: BTreeSet<String>,
    info_dirty: bool,
    /// Commands sent and not answered yet.
    pub pending: usize,
    /// The last answers per station, newest first.
    pub results: HashMap<String, VecDeque<ResultRecord>>,
    /// Response rules and faults the engine has active.
    pub rules: Rules,
    /// The server's connection security profile (0 to 3).
    pub security_profile: u8,
    /// Connections refused by the security policy since the UI started.
    pub auth_failures: u64,
    last_auth_notice: Option<DateTime<Utc>>,
    next_request_id: i32,
}

/// Input to [`update`].
#[derive(Debug)]
pub enum Msg {
    Key(crossterm::event::KeyEvent),
    Paste(String),
    Resize(u16, u16),
    /// A redraw tick with the current time.
    Tick(DateTime<Utc>),
    /// SIGINT/SIGTERM.
    Signal,
    Event(CsmsEvent),
    /// The event receiver fell behind and lost this many events.
    Lagged(u64),
    /// The event channel closed.
    Closed,
    /// The whole registry, from [`Effect::Resync`] or the start.
    Resynced {
        infos: Vec<StationInfo>,
        states: Vec<(String, Option<Box<StationState>>)>,
    },
    /// Everything about the station that was just selected, from [`Effect::Load`].
    Loaded {
        id: String,
        history: Vec<FrameEntry>,
        info: Option<Box<StationInfo>>,
        state: Option<Box<StationState>>,
    },
    /// Fresh states and info, from [`Effect::Refresh`].
    Refreshed {
        states: Vec<(String, Option<Box<StationState>>)>,
        info: Option<(String, Option<Box<StationInfo>>)>,
    },
    Notice(NoticeLevel, String),
    /// A command finished, from [`Effect::Send`].
    CommandDone {
        station: String,
        command: Command,
        outcome: CmdOutcome,
    },
    /// The active rules and faults, from [`Effect::ReadRules`] and after a change.
    Rules(Rules),
}

/// What the run loop must do after an [`update`].
#[derive(Debug, PartialEq)]
pub enum Effect {
    /// Read history, info and state of this station.
    Load(String),
    /// Read the state of these stations and the info of one.
    Refresh {
        states: Vec<String>,
        info: Option<String>,
    },
    /// Read the whole registry again.
    Resync,
    Copy {
        text: String,
        what: String,
    },
    /// Send a command to a station in the background; the answer comes back as
    /// [`Msg::CommandDone`].
    Send {
        station: String,
        command: Command,
    },
    /// Answer every `action` CALL with `payload` as a CALLRESULT.
    Respond {
        action: String,
        payload: serde_json::Value,
    },
    /// Fault the next `times` occurrences of `action`.
    Inject {
        action: String,
        kind: FaultKind,
        times: usize,
        payload: serde_json::Value,
    },
    RemoveRule(String),
    /// Drop every response rule and pending fault.
    ClearRules,
    /// Read the active rules and faults.
    ReadRules,
    Forget(String),
}

impl App {
    pub fn new(url: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            url: url.into(),
            stations: Vec::new(),
            station_filter: String::new(),
            matches: Vec::new(),
            selected: None,
            info: None,
            state: None,
            focus: Focus::Stations,
            mode: Mode::Normal,
            view: LogView::Station,
            station_log: LogStore::new(LOG_CAPACITY),
            all_log: LogStore::new(LOG_CAPACITY),
            tags: Tags::default(),
            station_tracker: ActionTracker::default(),
            all_tracker: ActionTracker::default(),
            dedupe: Vec::new(),
            dedupe_ticks: 0,
            frames_in: 0,
            frames_out: 0,
            notice: None,
            now,
            size: (80, 24),
            quit: false,
            dirty: true,
            stopped: false,
            dirty_states: BTreeSet::new(),
            info_dirty: false,
            pending: 0,
            results: HashMap::new(),
            rules: Rules::default(),
            security_profile: 0,
            auth_failures: 0,
            last_auth_notice: None,
            next_request_id: 1,
        }
    }

    /// A fresh request id for `GetLog`, `SignedUpdateFirmware` and `ReserveNow` forms.
    pub fn take_request_id(&mut self) -> i32 {
        let id = self.next_request_id;
        self.next_request_id = if id >= 1_000_000 { 1 } else { id + 1 };
        id
    }

    pub fn notify(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            level,
            ttl: NOTICE_TICKS,
        });
    }

    /// The log the log pane shows.
    pub fn log(&self) -> &LogStore {
        match self.view {
            LogView::Station => &self.station_log,
            LogView::All => &self.all_log,
        }
    }

    pub fn log_mut(&mut self) -> &mut LogStore {
        match self.view {
            LogView::Station => &mut self.station_log,
            LogView::All => &mut self.all_log,
        }
    }

    /// The station an all-stations log row belongs to.
    pub fn entry_station(&self, entry: &LogEntry) -> Option<&str> {
        match self.view {
            LogView::All => self.tags.get(entry.seq),
            LogView::Station => self.selected.as_deref(),
        }
    }

    pub fn connected_count(&self) -> usize {
        self.stations.iter().filter(|s| s.connected).count()
    }

    // ---- station list -----------------------------------------------------------------

    /// Rows the list shows (all stations, or those matching the filter).
    pub fn visible_len(&self) -> usize {
        if self.station_filter.is_empty() {
            self.stations.len()
        } else {
            self.matches.len()
        }
    }

    pub fn visible_at(&self, row: usize) -> Option<&Summary> {
        if self.station_filter.is_empty() {
            self.stations.get(row)
        } else {
            self.matches.get(row).and_then(|i| self.stations.get(*i))
        }
    }

    /// Row of the selected station in the list, if it is listed.
    pub fn cursor(&self) -> Option<usize> {
        let id = self.selected.as_deref()?;
        if self.station_filter.is_empty() {
            self.stations
                .binary_search_by(|s| s.id.as_str().cmp(id))
                .ok()
        } else {
            self.matches
                .iter()
                .position(|i| self.stations.get(*i).is_some_and(|s| s.id == id))
        }
    }

    fn refilter(&mut self) {
        self.matches.clear();
        if self.station_filter.is_empty() {
            return;
        }
        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(
            &self.station_filter,
            CaseMatching::Ignore,
            Normalization::Smart,
        );
        let mut buf = Vec::new();
        for (i, s) in self.stations.iter().enumerate() {
            if pattern
                .score(Utf32Str::new(&s.id, &mut buf), &mut matcher)
                .is_some()
            {
                self.matches.push(i);
            }
        }
    }

    /// Applies a changed filter text and keeps the selection on a listed station.
    pub fn set_station_filter(&mut self, text: &str, fx: &mut Vec<Effect>) {
        text.clone_into(&mut self.station_filter);
        self.refilter();
        if self.cursor().is_none()
            && let Some(first) = self.visible_at(0).map(|s| s.id.clone())
        {
            self.select(Some(first), fx);
        }
    }

    /// Moves the list cursor by `delta` rows.
    pub fn move_cursor(&mut self, delta: isize, fx: &mut Vec<Effect>) {
        let len = self.visible_len();
        if len == 0 {
            return;
        }
        let next = self
            .cursor()
            .map_or(0, |c| c.saturating_add_signed(delta).min(len - 1));
        self.select_row(next, fx);
    }

    pub fn select_row(&mut self, row: usize, fx: &mut Vec<Effect>) {
        let Some(id) = self.visible_at(row).map(|s| s.id.clone()) else {
            return;
        };
        self.select(Some(id), fx);
    }

    /// Selects a station and asks for everything about it.
    fn select(&mut self, id: Option<String>, fx: &mut Vec<Effect>) {
        if self.selected == id {
            return;
        }
        self.selected.clone_from(&id);
        self.info = None;
        self.state = None;
        self.reset_station_log();
        self.dirty = true;
        if let Some(id) = id {
            fx.push(Effect::Load(id));
        }
    }

    fn reset_station_log(&mut self) {
        let (filter, search) = (self.station_log.filter, self.station_log.search.clone());
        self.station_log = LogStore::new(LOG_CAPACITY);
        self.station_log.filter = filter;
        self.station_log.search = search;
        self.station_tracker = ActionTracker::default();
        self.dedupe.clear();
    }

    pub(crate) fn summary_mut(&mut self, id: &str) -> Option<&mut Summary> {
        let i = self
            .stations
            .binary_search_by(|s| s.id.as_str().cmp(id))
            .ok()?;
        self.stations.get_mut(i)
    }

    /// Adds a station to the list if it is new; returns whether it was.
    fn ensure_station(&mut self, id: &str, connected: bool) -> bool {
        match self.stations.binary_search_by(|s| s.id.as_str().cmp(id)) {
            Ok(_) => false,
            Err(pos) => {
                self.stations
                    .insert(pos, Summary::new(id.to_owned(), connected));
                if !self.station_filter.is_empty() {
                    self.refilter();
                }
                true
            }
        }
    }

    fn remove_station(&mut self, id: &str, fx: &mut Vec<Effect>) {
        let Ok(pos) = self.stations.binary_search_by(|s| s.id.as_str().cmp(id)) else {
            return;
        };
        let was_selected = self.selected.as_deref() == Some(id);
        let row = self.cursor();
        self.stations.remove(pos);
        self.forget_results(id);
        self.refilter();
        self.dirty_states.remove(id);
        if was_selected {
            self.selected = None;
            self.info = None;
            self.state = None;
            self.reset_station_log();
            let len = self.visible_len();
            let next = row.unwrap_or(0).min(len.saturating_sub(1));
            if let Some(next) = self.visible_at(next).map(|s| s.id.clone()) {
                self.select(Some(next), fx);
            }
        }
    }

    // ---- log --------------------------------------------------------------------------

    fn push_frame(&mut self, station: &str, direction: CsmsDir, frame: &Frame, into_station: bool) {
        let dir = match direction {
            CsmsDir::In => Direction::In,
            CsmsDir::Out => Direction::Out,
        };
        let key = format!("{station}{KEY_SEP}{}", frame.id());
        let record = self.all_tracker.record(self.now, dir, key, frame);
        self.all_log.push(Event::Frame(record));
        self.tags.push(self.all_log.capacity(), station.to_owned());
        if into_station {
            let record = self
                .station_tracker
                .record(self.now, dir, frame.id().to_owned(), frame);
            self.station_log.push(Event::Frame(record));
        }
    }

    fn push_line(&mut self, station: &str, level: LogLevel, text: String) {
        let record = LogRecord {
            ts: self.now,
            level,
            text,
        };
        self.all_log.push(Event::Log(record.clone()));
        self.tags.push(self.all_log.capacity(), station.to_owned());
        if self.selected.as_deref() == Some(station) {
            self.station_log.push(Event::Log(record));
        }
    }

    fn load_history(&mut self, history: Vec<FrameEntry>) {
        self.reset_station_log();
        for entry in &history {
            let dir = match entry.direction {
                CsmsDir::In => Direction::In,
                CsmsDir::Out => Direction::Out,
            };
            let key = entry.frame.id().to_owned();
            let record = self
                .station_tracker
                .record(entry.ts, dir, key, &entry.frame);
            self.station_log.push(Event::Frame(record));
        }
        let skip = history.len().saturating_sub(DEDUPE_ENTRIES);
        self.dedupe = history.into_iter().skip(skip).collect();
        self.dedupe_ticks = DEDUPE_TICKS;
    }

    /// Whether this live frame is one the history load already contained.
    fn is_duplicate(&mut self, direction: CsmsDir, frame: &Frame) -> bool {
        match self
            .dedupe
            .iter()
            .position(|e| e.direction == direction && e.frame == *frame)
        {
            Some(i) => {
                self.dedupe.remove(i);
                true
            }
            None => false,
        }
    }

    /// The text of a log row for copying.
    pub fn entry_text(&self, entry: &LogEntry) -> String {
        let ts = entry.ts.format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let station = self
            .entry_station(entry)
            .map_or_else(String::new, |s| format!(" [{s}]"));
        match entry.frame() {
            Some(f) => format!(
                "{ts}{station} {} {}",
                if f.direction == Direction::In {
                    "<-"
                } else {
                    "->"
                },
                f.frame
            ),
            None => format!("{ts}{station} {}", entry.pretty()),
        }
    }

    /// Opens the detail popup for the selected log row.
    pub fn open_detail(&mut self) {
        let log = self.log();
        let Some(entry) = log.selected() else {
            return;
        };
        let mut lines = vec![self.entry_header(entry)];
        lines.push(String::new());
        lines.extend(entry.pretty().lines().map(kirby_ocpp_tui::text::sanitize));
        if let Some(peer) = entry.peer.and_then(|s| log.get(s)) {
            lines.push(String::new());
            lines.push(format!(
                "--- {} ---",
                if peer.seq > entry.seq {
                    "answer"
                } else {
                    "request"
                }
            ));
            lines.push(self.entry_header(peer));
            lines.extend(peer.pretty().lines().map(kirby_ocpp_tui::text::sanitize));
        }
        let title = entry
            .frame()
            .and_then(|f| f.action.clone())
            .unwrap_or_else(|| "log line".to_owned());
        let detail = Detail {
            title,
            copy: entry.pretty(),
            lines,
            scroll: 0,
        };
        self.mode = Mode::Detail(detail);
    }

    fn entry_header(&self, e: &LogEntry) -> String {
        let ts = e.ts.format("%Y-%m-%d %H:%M:%S%.3f UTC");
        let station = self.entry_station(e).map_or_else(String::new, |s| {
            format!("  station={}", kirby_ocpp_tui::text::sanitize(s))
        });
        match e.frame() {
            Some(f) => {
                let rtt = e
                    .rtt_ms
                    .map_or_else(String::new, |ms| format!("  ({ms} ms)"));
                let id = f
                    .msg_id
                    .as_deref()
                    .map_or("-", |k| k.rsplit(KEY_SEP).next().unwrap_or(k));
                format!(
                    "{ts}  {}  id={}{rtt}{station}",
                    if f.direction == Direction::In {
                        "station -> CSMS"
                    } else {
                        "CSMS -> station"
                    },
                    kirby_ocpp_tui::text::sanitize(id)
                )
            }
            None => format!("{ts}{station}"),
        }
    }
}

/// Label of a log filter in this UI (the emulator's `csms` filter means "received").
pub fn filter_label(filter: LogFilter) -> &'static str {
    match filter {
        LogFilter::Csms => "from stations",
        LogFilter::Station => "lines",
        other => other.label(),
    }
}

/// Filters this UI cycles through.
pub fn next_filter(filter: LogFilter) -> LogFilter {
    match filter {
        LogFilter::All => LogFilter::Calls,
        LogFilter::Calls => LogFilter::Errors,
        LogFilter::Errors => LogFilter::Csms,
        LogFilter::Csms | LogFilter::Station => LogFilter::All,
    }
}

fn reason_text(reason: DisconnectReason) -> &'static str {
    match reason {
        DisconnectReason::Closed => "closed",
        DisconnectReason::Replaced => "replaced by a new connection",
        DisconnectReason::Shutdown => "server shutdown",
        DisconnectReason::Injected => "closed by an injected fault",
        DisconnectReason::Forgotten => "forgotten",
        DisconnectReason::PingTimeout => "ping timeout",
    }
}

/// The only way the model changes.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    let mut fx = Vec::new();
    if !matches!(msg, Msg::Tick(_)) {
        app.dirty = true;
    }
    match msg {
        Msg::Key(key) => {
            if key.kind != crossterm::event::KeyEventKind::Release {
                keys::handle(app, key, &mut fx);
            }
        }
        Msg::Paste(text) => keys::paste(app, &text, &mut fx),
        Msg::Resize(w, h) => app.size = (w, h),
        Msg::Signal => app.quit = true,
        Msg::Tick(now) => tick(app, now, &mut fx),
        Msg::Event(event) => on_event(app, event, &mut fx),
        Msg::Lagged(n) => {
            app.notify(NoticeLevel::Error, format!("{n} events dropped, resyncing"));
            fx.push(Effect::Resync);
        }
        Msg::Closed => {
            app.stopped = true;
            app.notify(NoticeLevel::Error, "server stopped");
        }
        Msg::Resynced { infos, states } => resync(app, &infos, states, &mut fx),
        Msg::Loaded {
            id,
            history,
            info,
            state,
        } => {
            if app.selected.as_deref() == Some(id.as_str()) {
                app.load_history(history);
                app.info = info.map(|b| *b);
                if let Some(state) = state {
                    if let Some(s) = app.summary_mut(&id) {
                        s.apply(&state);
                    }
                    app.state = Some(*state);
                }
            }
        }
        Msg::Refreshed { states, info } => {
            for (id, state) in states {
                let Some(state) = state else { continue };
                if let Some(s) = app.summary_mut(&id) {
                    s.apply(&state);
                }
                if app.selected.as_deref() == Some(id.as_str()) {
                    app.state = Some(*state);
                }
            }
            if let Some((id, info)) = info
                && app.selected.as_deref() == Some(id.as_str())
            {
                app.info = info.map(|b| *b);
            }
        }
        Msg::Notice(level, text) => app.notify(level, text),
        Msg::CommandDone {
            station,
            command,
            outcome,
        } => app.command_done(&station, &command, &outcome),
        Msg::Rules(rules) => app.rules = rules,
    }
    fx
}

fn tick(app: &mut App, now: DateTime<Utc>, fx: &mut Vec<Effect>) {
    if now.timestamp() != app.now.timestamp() {
        app.dirty = true;
        // Faults are used up by traffic, so watch them while any is active.
        if !app.rules.is_empty() {
            fx.push(Effect::ReadRules);
        }
    }
    app.now = now;
    if let Some(n) = &mut app.notice {
        n.ttl = n.ttl.saturating_sub(1);
        if n.ttl == 0 {
            app.notice = None;
            app.dirty = true;
        }
    }
    app.dedupe_ticks = app.dedupe_ticks.saturating_sub(1);
    if app.dedupe_ticks == 0 {
        app.dedupe.clear();
    }
    if !app.dirty_states.is_empty() || app.info_dirty {
        let states = std::mem::take(&mut app.dirty_states).into_iter().collect();
        let info = if app.info_dirty {
            app.selected.clone()
        } else {
            None
        };
        app.info_dirty = false;
        fx.push(Effect::Refresh { states, info });
    }
}

fn on_event(app: &mut App, event: CsmsEvent, fx: &mut Vec<Effect>) {
    match event {
        CsmsEvent::Connected { station } => {
            if !app.ensure_station(&station, true)
                && let Some(s) = app.summary_mut(&station)
            {
                s.connected = true;
            }
            app.push_line(&station, LogLevel::Success, "connected".to_owned());
            if app.selected.is_none() {
                app.select(Some(station.clone()), fx);
            }
            if app.selected.as_deref() == Some(station.as_str()) {
                app.info_dirty = true;
            }
        }
        CsmsEvent::Disconnected { station, reason } => {
            app.ensure_station(&station, false);
            if let Some(s) = app.summary_mut(&station) {
                s.connected = false;
                s.replaced = reason == DisconnectReason::Replaced;
            }
            app.push_line(
                &station,
                LogLevel::Warn,
                format!("disconnected: {}", reason_text(reason)),
            );
            if app.selected.as_deref() == Some(station.as_str()) {
                app.info_dirty = true;
            }
        }
        CsmsEvent::Frame {
            station,
            direction,
            frame,
        } => {
            match direction {
                CsmsDir::In => app.frames_in += 1,
                CsmsDir::Out => app.frames_out += 1,
            }
            let selected = app.selected.as_deref() == Some(station.as_str());
            // A frame the history load already contained stays out of the station log (the
            // all-stations log never replays history).
            let into_station = selected && !app.is_duplicate(direction, &frame);
            app.push_frame(&station, direction, &frame, into_station);
            if selected && direction == CsmsDir::In {
                app.info_dirty = true;
            }
        }
        CsmsEvent::Malformed { station, text } => {
            app.push_line(
                &station,
                LogLevel::Warn,
                format!("malformed frame: {}", kirby_ocpp_tui::text::sanitize(&text)),
            );
        }
        CsmsEvent::Forgotten { station } => {
            app.push_line(&station, LogLevel::Warn, "forgotten".to_owned());
            app.remove_station(&station, fx);
        }
        CsmsEvent::StateChanged { station } => {
            app.ensure_station(&station, false);
            app.dirty_states.insert(station);
        }
        CsmsEvent::AuthFailed {
            station,
            remote_addr,
            reason,
        } => {
            app.auth_failures += 1;
            app.dirty = true;
            // At most one notice per second, however many connections are refused.
            let due = app
                .last_auth_notice
                .is_none_or(|t| app.now - t >= chrono::Duration::seconds(1));
            if due {
                app.last_auth_notice = Some(app.now);
                let who = station.map_or_else(
                    || "unknown station".to_owned(),
                    |s| kirby_ocpp_tui::text::truncate(&kirby_ocpp_tui::text::sanitize(&s), 40),
                );
                let reason = kirby_ocpp_tui::text::sanitize(&reason);
                app.notify(
                    NoticeLevel::Error,
                    format!("refused {who} from {remote_addr}: {reason}"),
                );
            }
        }
    }
}

fn resync(
    app: &mut App,
    infos: &[StationInfo],
    states: Vec<(String, Option<Box<StationState>>)>,
    fx: &mut Vec<Effect>,
) {
    let replaced: BTreeSet<String> = app
        .stations
        .iter()
        .filter(|s| s.replaced)
        .map(|s| s.id.clone())
        .collect();
    app.stations = infos
        .iter()
        .map(|i| {
            let mut s = Summary::new(i.id.clone(), i.connected);
            s.replaced = replaced.contains(&i.id);
            s
        })
        .collect();
    app.stations.sort_by(|a, b| a.id.cmp(&b.id));
    for (id, state) in states {
        if let (Some(state), Some(s)) = (state, app.summary_mut(&id)) {
            s.apply(&state);
        }
    }
    app.refilter();
    app.dirty_states.clear();
    let keep = app.selected.clone().filter(|id| {
        app.stations
            .binary_search_by(|s| s.id.as_str().cmp(id))
            .is_ok()
    });
    // Reselect even the same station: its log and state may have missed events.
    app.selected = None;
    let next = keep.or_else(|| app.visible_at(0).map(|s| s.id.clone()));
    app.select(next, fx);
}
