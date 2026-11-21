//! The model of the OCPP 2.x UI: state, messages and effects. [`super::update2`] is the only
//! thing that changes an [`App2`].

use std::time::Instant;

use crossterm::event::KeyEvent;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::scenario::{Scenario, ScenarioProgress, ScenarioReport, builtin};
use kirby_ocpp_core::station::Event;
use kirby_ocpp_core::station2::{Command2, EvseSnapshot, Outcome2, Snapshot2};
use kirby_ocpp_core::timefmt::TimeMode;

use super::actions::{ActionDef2, ActionId2, Context2, Tab2, catalog2, disabled_reason2};
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

impl Mode2 {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Palette(_) => "palette",
            Self::Form(_) => "form",
            Self::Choice(_) => "choice",
            Self::Help => "help",
            Self::Detail(_) => "detail",
            Self::Confirm(_) => "confirm",
            Self::DeviceModel(_) => "device model",
            Self::Pending(_) => "pending",
        }
    }
}

/// Something the event loop must do on behalf of the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect2 {
    CancelCall {
        action: String,
    },
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
    /// The OCPP version of the station (2.0.1 or 2.1).
    pub protocol: Protocol,
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
    /// The station is one of several in a fleet: quitting asks first and the help lists the
    /// selector keys.
    pub in_fleet: bool,
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
        let mut catalog = catalog2(&scenarios);
        if protocol == Protocol::V21 {
            // The catalogue names the version in a few descriptions.
            for def in &mut catalog {
                def.desc = def.desc.replace("OCPP 2.0.1", "OCPP 2.1");
            }
        }
        let evse = snapshot.evses.first().map_or(0, |e| e.id);
        Self {
            protocol,
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
            catalog,
            scenarios,
            scenario: ScenarioView::default(),
            dirty: true,
            quit: false,
            quit_prev: None,
            pending: Vec::new(),
            now: Instant::now(),
            mono: false,
            in_fleet: false,
            time: TimeMode::Utc,
            security_events_sent: 0,
        }
    }

    /// Why quitting should be confirmed, if it should.
    pub fn quit_reason(&self) -> Option<&'static str> {
        if self.scenario.running {
            Some("a scenario is running")
        } else if self.snapshot.evses.iter().any(|e| e.transaction.is_some()) {
            Some("a transaction is active")
        } else if self.in_fleet {
            Some("several stations are running")
        } else {
            None
        }
    }

    /// Quits at once, or opens the confirmation when something would be lost.
    pub fn request_quit(&mut self) {
        match self.quit_reason() {
            Some(why) => {
                self.mode = Mode2::Confirm(Confirm2 {
                    message: format!("Quit? {}", capitalize(why)),
                    action: ConfirmAction2::Quit,
                });
            }
            None => self.quit = true,
        }
    }

    pub fn notify(&mut self, level: NoticeLevel, text: impl Into<String>) {
        // Notices may carry peer-controlled text (CALLERROR descriptions).
        let text: String = text.into();
        self.notice = Some(Notice::new(&text, level));
    }

    /// The selected EVSE, if the station has it.
    pub fn selected_evse(&self) -> Option<&EvseSnapshot> {
        self.snapshot.evse(self.evse)
    }

    /// Catalogue indices of the active tab's actions.
    pub fn tab_actions(&self) -> Vec<usize> {
        self.catalog
            .iter()
            .enumerate()
            .filter(|(_, a)| a.tab == self.tab)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn selected_action(&self) -> Option<&ActionDef2> {
        let list = self.tab_actions();
        let i = *list.get(self.action_sel[self.tab.index()])?;
        self.catalog.get(i)
    }

    /// Why `id` cannot run right now; `None` when it can.
    pub fn reason_for2(&self, id: ActionId2) -> Option<String> {
        disabled_reason2(
            id,
            &Context2 {
                snapshot: &self.snapshot,
                evse: self.evse,
                scenario_running: self.scenario.running,
                log_has_selection: self.log.selected().is_some(),
                log_visible: self.log.visible_count(),
            },
        )
    }

    pub fn set_tab(&mut self, tab: Tab2) {
        self.tab = tab;
        self.focus = if tab == Tab2::Logs {
            Focus::Log
        } else {
            Focus::Actions
        };
    }

    /// Moves the EVSE selection by `delta` rows, stopping at the ends.
    pub fn move_evse(&mut self, delta: isize) {
        let n = self.snapshot.evses.len();
        if n == 0 {
            return;
        }
        let cur = self
            .snapshot
            .evses
            .iter()
            .position(|e| e.id == self.evse)
            .unwrap_or(0);
        let next = cur.saturating_add_signed(delta).min(n - 1);
        self.evse = self.snapshot.evses[next].id;
    }

    /// Moves the action selection of the active tab, wrapping around.
    pub fn move_action(&mut self, delta: isize) {
        let n = self.tab_actions().len();
        if n > 0 {
            let sel = &mut self.action_sel[self.tab.index()];
            *sel = (*sel + n).wrapping_add_signed(delta) % n;
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}
