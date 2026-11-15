//! The model of the OCPP 2.x UI: state, messages and effects. [`super::update2`] is the only
//! thing that changes an [`App2`].

use std::time::Instant;

use crossterm::event::KeyEvent;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::scenario::{Scenario, ScenarioProgress, ScenarioReport, builtin};
use kirby_ocpp_core::station::Event;
use kirby_ocpp_core::station2::{Command2, Outcome2, Snapshot2};
use kirby_ocpp_core::timefmt::TimeMode;

use super::actions::{ActionDef2, Tab2, catalog2};
use super::browser::DeviceBrowser;
use crate::app::{Detail, Focus, Notice, NoticeLevel, PendingCall, ScenarioView};
use crate::forms::{Choice, Form};
use crate::logview::LogStore;
use crate::palette::Palette;

/// What a confirmation question does when answered yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction2 {
    ClearLog,
    Quit,
}

/// A yes/no question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm2 {
    pub message: String,
    pub action: ConfirmAction2,
}

/// Exactly one mode owns the keyboard at a time: the 1.6 modes except `Browser` (the
/// 1.6 configuration, local list and profile pages), plus the device-model browser.
#[derive(Debug)]
pub enum Mode2 {
    Normal,
    Palette(Palette),
    Form(Form),
    Choice(Choice),
    Help,
    Detail(Detail),
    Confirm(Confirm2),
    DeviceModel(DeviceBrowser),
    /// The commands still waiting for the CSMS; the index is the selected row.
    Pending(usize),
}

/// Something the event loop must do on behalf of the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect2 {
    /// Sends `commands` one after the other (the next only after the previous one finished)
    /// and reports the outcome of the last one, or the first failure, as
    /// [`Msg2::CommandDone`].
    Send {
        label: String,
        commands: Vec<Command2>,
        /// Show the CALLRESULT payload in a detail view.
        show_response: bool,
    },
    RunScenario(Box<Scenario>),
    RunScenarioFile(String),
    CancelScenario,
    Copy {
        text: String,
        what: String,
    },
    WriteFile {
        path: String,
        contents: String,
    },
}

/// Everything that can happen to the model.
#[derive(Debug)]
pub enum Msg2 {
    Key(KeyEvent),
    Paste(String),
    Resize(u16, u16),
    Tick,
    Snapshot(Box<Snapshot2>),
    Event(Event),
    Scenario(Box<ScenarioProgress>),
    ScenarioDone(Box<ScenarioReport>),
    /// A scenario could not start (already running, unreadable file, ...).
    ScenarioError(String),
    /// An [`Effect2::Send`] finished: the outcome of its last command, or the error of the
    /// first one that failed.
    CommandDone {
        label: String,
        show_response: bool,
        result: Result<Outcome2, String>,
    },
    Notice(NoticeLevel, String),
    /// SIGINT/SIGTERM.
    Signal,
}

#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent view flags (dirty, quit, mono) of one model"
)]
pub struct App2 {
    pub size: (u16, u16),
    pub snapshot: Snapshot2,
    pub url: String,
    pub vendor: String,
    pub tab: Tab2,
    /// The left pane is the EVSE list; `Focus::Connectors` stands for it.
    pub focus: Focus,
    /// The selected EVSE id (`1..=n`), 0 while the station has none.
    pub evse: u32,
    /// The selected action per tab, indexed by [`Tab2::index`].
    pub action_sel: [usize; Tab2::ALL.len()],
    pub mode: Mode2,
    /// The device-model browser while a form or detail opened from it is on screen; it comes
    /// back when that one closes.
    pub suspended: Option<DeviceBrowser>,
    pub log: LogStore,
    pub notice: Option<Notice>,
    pub scenarios: Vec<Scenario>,
    pub catalog: Vec<ActionDef2>,
    pub scenario: ScenarioView,
    pub dirty: bool,
    pub quit: bool,
    /// The mode that was on screen when Ctrl-C opened the quit question; it comes back when
    /// the question is declined.
    pub quit_prev: Option<Mode2>,
    /// Commands sent to the station that have not completed yet, oldest first.
    pub pending: Vec<PendingCall>,
    /// The time of the last tick (what the elapsed times of pending commands count to).
    pub now: Instant,
    /// Draw without color; states then also carry text markers.
    pub mono: bool,
    /// Clock of the log rows.
    pub time: TimeMode,
    /// Outbound `SecurityEventNotification` frames since the UI started (the snapshot has no
    /// list of them).
    pub security_events_sent: u64,
}

impl App2 {
    pub fn new(snapshot: Snapshot2, url: impl Into<String>, vendor: impl Into<String>) -> Self {
        let protocol = Protocol::from_token(&snapshot.protocol).unwrap_or(Protocol::V201);
        let scenarios = builtin::all_for(protocol);
        let evse = snapshot.evses.first().map_or(0, |e| e.id);
        Self {
            size: (120, 40),
            snapshot,
            url: url.into(),
            vendor: vendor.into(),
            tab: Tab2::Station,
            focus: Focus::Actions,
            evse,
            action_sel: [0; Tab2::ALL.len()],
            mode: Mode2::Normal,
            suspended: None,
            log: LogStore::default(),
            notice: None,
            catalog: catalog2(&scenarios),
            scenarios,
            scenario: ScenarioView::default(),
            dirty: true,
            quit: false,
            quit_prev: None,
            pending: Vec::new(),
            now: Instant::now(),
            mono: false,
            time: TimeMode::Utc,
            security_events_sent: 0,
        }
    }
}
