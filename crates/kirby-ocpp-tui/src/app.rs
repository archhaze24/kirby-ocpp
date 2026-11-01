//! The UI model and its pure update function.
//!
//! [`update`] never does I/O: it mutates the [`App`] and returns [`Effect`]s that the
//! event loop executes (and whose results come back as [`Msg`]s).

use crossterm::event::KeyEvent;
use kirby_ocpp_core::ids::{ConnectorId, IdTag};
use kirby_ocpp_core::scenario::{
    Scenario, ScenarioProgress, ScenarioReport, StepReport, StepStatus, builtin,
};
use kirby_ocpp_core::station::{Command, Event, Outcome, Snapshot};

use crate::actions::{self, ActionDef, ActionId, Context, Tab};
use crate::browser::{Browser, Page};
use crate::forms::{Choice, ChoiceKind, Form, Submitted};
use crate::logview::{LogEntry, LogStore};
use crate::palette::Palette;

/// Notices stay for this many ticks (about three seconds at 30 fps).
const NOTICE_TICKS: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Connectors,
    Actions,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Success,
    Error,
}

/// A transient message for the status line (never written to the log).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub level: NoticeLevel,
    ttl: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    /// Text `c` copies.
    pub copy: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    ClearLog,
    ClearLocalList,
    RemoveLocalEntry(IdTag),
    ClearProfiles,
    RemoveProfile(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub message: String,
    pub action: ConfirmAction,
}

/// Exactly one mode owns the keyboard at a time.
#[derive(Debug)]
pub enum Mode {
    Normal,
    Palette(Palette),
    Form(Form),
    Choice(Choice),
    Help,
    Detail(Detail),
    Confirm(Confirm),
    /// Configuration keys, local list and charging profiles (see [`crate::browser`]).
    Browser(Browser),
}

impl Mode {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Palette(_) => "palette",
            Self::Form(_) => "form",
            Self::Choice(_) => "choice",
            Self::Help => "help",
            Self::Detail(_) => "detail",
            Self::Confirm(_) => "confirm",
            Self::Browser(_) => "browser",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Running,
    Passed(u64),
    Failed(String),
    Skipped,
}

/// Progress of the current (or last) scenario run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScenarioView {
    pub name: String,
    pub steps: Vec<(String, StepState)>,
    pub running: bool,
    pub report: Option<ScenarioReport>,
}

/// Something the event loop must do on behalf of the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Send {
        label: String,
        command: Command,
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
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Resize(u16, u16),
    Tick,
    Snapshot(Box<Snapshot>),
    Event(Event),
    Scenario(ScenarioProgress),
    ScenarioDone(Box<ScenarioReport>),
    /// A scenario could not start (already running, unreadable file, ...).
    ScenarioError(String),
    CommandDone {
        label: String,
        show_response: bool,
        result: Result<Outcome, String>,
    },
    Notice(NoticeLevel, String),
    /// SIGINT/SIGTERM.
    Signal,
}

#[derive(Debug)]
pub struct App {
    pub size: (u16, u16),
    pub snapshot: Snapshot,
    pub url: String,
    pub vendor: String,
    pub tab: Tab,
    pub focus: Focus,
    pub connector: ConnectorId,
    pub action_sel: [usize; 7],
    pub mode: Mode,
    /// The browser while a form, confirmation or detail opened from it is on screen; it comes
    /// back when that one closes.
    pub suspended: Option<Browser>,
    pub log: LogStore,
    pub notice: Option<Notice>,
    pub scenarios: Vec<Scenario>,
    pub catalog: Vec<ActionDef>,
    pub scenario: ScenarioView,
    pub dirty: bool,
    pub quit: bool,
}

impl App {
    pub fn new(snapshot: Snapshot, url: impl Into<String>, vendor: impl Into<String>) -> Self {
        let scenarios = builtin::all();
        let connector = snapshot
            .connectors
            .first()
            .map_or(ConnectorId::STATION, |c| c.id);
        Self {
            size: (120, 40),
            snapshot,
            url: url.into(),
            vendor: vendor.into(),
            tab: Tab::Station,
            focus: Focus::Actions,
            connector,
            action_sel: [0; 7],
            mode: Mode::Normal,
            suspended: None,
            log: LogStore::default(),
            notice: None,
            catalog: actions::catalog(&scenarios),
            scenarios,
            scenario: ScenarioView::default(),
            dirty: true,
            quit: false,
        }
    }

    pub fn notify(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            level,
            ttl: NOTICE_TICKS,
        });
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

    pub fn selected_action(&self) -> Option<&ActionDef> {
        let list = self.tab_actions();
        let i = *list.get(self.action_sel[self.tab.index()])?;
        self.catalog.get(i)
    }

    pub fn reason_for(&self, id: ActionId) -> Option<String> {
        actions::disabled_reason(
            id,
            &Context {
                snapshot: &self.snapshot,
                connector: self.connector,
                scenario_running: self.scenario.running,
                log_has_selection: self.log.selected().is_some(),
                log_visible: self.log.visible_count(),
            },
        )
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.focus = if tab == Tab::Logs {
            Focus::Log
        } else {
            Focus::Actions
        };
    }

    pub fn move_connector(&mut self, delta: isize) {
        let n = self.snapshot.connectors.len();
        if n == 0 {
            return;
        }
        let cur = self
            .snapshot
            .connectors
            .iter()
            .position(|c| c.id == self.connector)
            .unwrap_or(0);
        let next = cur.saturating_add_signed(delta).min(n - 1);
        self.connector = self.snapshot.connectors[next].id;
    }

    pub fn move_action(&mut self, delta: isize) {
        let n = self.tab_actions().len();
        if n > 0 {
            let sel = &mut self.action_sel[self.tab.index()];
            *sel = (*sel + n).wrapping_add_signed(delta) % n;
        }
    }

    fn max_connector(&self) -> u32 {
        self.snapshot.connectors.last().map_or(1, |c| c.id.get())
    }

    /// Runs an action, or explains why it cannot run.
    pub fn run_action(&mut self, id: ActionId, fx: &mut Vec<Effect>) {
        if let Some(reason) = self.reason_for(id) {
            self.notify(NoticeLevel::Error, reason);
            return;
        }
        let c = self.connector;
        let snap_conn = self.snapshot.connector(c).cloned();
        let send = |fx: &mut Vec<Effect>, command: Command| {
            let label = command.name().to_owned();
            fx.push(Effect::Send {
                label,
                command,
                show_response: false,
            });
        };
        match id {
            ActionId::Boot => send(fx, Command::Boot),
            ActionId::Heartbeat => send(fx, Command::Heartbeat),
            ActionId::Reconnect => send(fx, Command::Reconnect),
            ActionId::Disconnect => send(fx, Command::Disconnect),
            ActionId::Connect => send(fx, Command::Connect),
            ActionId::AddConnector => send(fx, Command::AddConnector),
            ActionId::RuleSet => self.mode = Mode::Form(Form::rule()),
            ActionId::RulesClear => send(fx, Command::ClearResponseRules { action: None }),
            ActionId::BrowseConfig => self.mode = Mode::Browser(Browser::new(Page::Config)),
            ActionId::BrowseLocalList => self.mode = Mode::Browser(Browser::new(Page::LocalList)),
            ActionId::BrowseProfiles => self.mode = Mode::Browser(Browser::new(Page::Profiles)),
            ActionId::PlugToggle => {
                if snap_conn.is_some_and(|s| s.ev_connected) {
                    send(fx, Command::Unplug { connector: c });
                } else {
                    send(fx, Command::PlugIn { connector: c });
                }
            }
            ActionId::CycleStatus => send(fx, Command::CycleConnectorStatus { connector: c }),
            ActionId::StatusNotification => send(fx, Command::StatusNotification { connector: c }),
            ActionId::SetStatus => self.mode = Mode::Choice(Choice::new(ChoiceKind::SetStatus, c)),
            ActionId::FaultToggle => {
                use kirby_ocpp_core::ocpp::types::ChargePointErrorCode::NoError;
                if snap_conn.is_some_and(|s| s.error_code != NoError) {
                    send(fx, Command::ClearFault { connector: c });
                } else {
                    self.mode = Mode::Choice(Choice::new(ChoiceKind::Fault, c));
                }
            }
            ActionId::Authorize => self.mode = Mode::Form(Form::authorize()),
            ActionId::StartTx => {
                self.mode = Mode::Form(Form::start_tx(c, self.max_connector()));
            }
            ActionId::StopTx => self.mode = Mode::Choice(Choice::new(ChoiceKind::StopReason, c)),
            ActionId::Meter120 => send(
                fx,
                Command::MeterValues {
                    connector: c,
                    wh_delta: 120,
                    measurands: None,
                },
            ),
            ActionId::MeterCustom => {
                self.mode = Mode::Form(Form::meter_custom(c, self.max_connector()));
            }
            ActionId::DiagOutcome => {
                self.mode = Mode::Choice(Choice::new(ChoiceKind::DiagOutcome, c));
            }
            ActionId::FwOutcome => self.mode = Mode::Choice(Choice::new(ChoiceKind::FwOutcome, c)),
            ActionId::DiagStatus => {
                self.mode = Mode::Choice(Choice::new(ChoiceKind::DiagStatus, c));
            }
            ActionId::FwStatus => self.mode = Mode::Choice(Choice::new(ChoiceKind::FwStatus, c)),
            ActionId::DataTransfer => self.mode = Mode::Form(Form::data_transfer(&self.vendor)),
            ActionId::VendorPing => fx.push(Effect::Send {
                label: "vendor ping".to_owned(),
                command: Command::DataTransfer {
                    vendor_id: self.vendor.clone(),
                    message_id: None,
                    data: Some("ping".to_owned()),
                },
                show_response: true,
            }),
            ActionId::RawCall => self.mode = Mode::Form(Form::raw(Some("Heartbeat"))),
            ActionId::LogCopySelected => {
                if let Some(e) = self.log.selected() {
                    fx.push(Effect::Copy {
                        text: e.pretty(),
                        what: "selected row".to_owned(),
                    });
                }
            }
            ActionId::LogCopyVisible => {
                let text: Vec<String> = self.log.visible().iter().map(|e| event_line(e)).collect();
                fx.push(Effect::Copy {
                    text: text.join("\n"),
                    what: format!("{} rows", text.len()),
                });
            }
            ActionId::LogFilter => {
                let next = self.log.filter.next();
                self.log.set_filter(next);
                self.notify(NoticeLevel::Info, format!("log filter: {}", next.label()));
            }
            ActionId::LogSearch => self.mode = Mode::Form(Form::search(&self.log.search)),
            ActionId::LogPause => {
                self.log.toggle_follow();
                let state = if self.log.follow {
                    "following"
                } else {
                    "paused"
                };
                self.notify(NoticeLevel::Info, format!("log {state}"));
            }
            ActionId::LogBottom => self.log.follow_tail(),
            ActionId::LogExport => {
                let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
                let path = format!(
                    "./kirby-ocpp-{}-{stamp}.jsonl",
                    export_name_part(&self.snapshot.charge_point_id)
                );
                self.mode = Mode::Form(Form::export(&path));
            }
            ActionId::LogClear => {
                self.mode = Mode::Confirm(Confirm {
                    message: format!("Clear all {} log rows?", self.log.len()),
                    action: ConfirmAction::ClearLog,
                });
            }
            ActionId::RunScenario(i) => {
                if let Some(s) = self.scenarios.get(i).cloned() {
                    self.start_scenario(s, fx);
                }
            }
            ActionId::LoadScenario => self.mode = Mode::Form(Form::load_scenario()),
            ActionId::CancelScenario => {
                fx.push(Effect::CancelScenario);
                self.notify(NoticeLevel::Info, "cancelling scenario...");
            }
        }
    }

    fn start_scenario(&mut self, s: Scenario, fx: &mut Vec<Effect>) {
        self.scenario = ScenarioView {
            name: s.name.clone(),
            steps: s
                .steps
                .iter()
                .map(|st| (st.label(), StepState::Pending))
                .collect(),
            running: true,
            report: None,
        };
        self.notify(NoticeLevel::Info, format!("running scenario {}", s.name));
        fx.push(Effect::RunScenario(Box::new(s)));
    }

    /// Applies a validated form.
    pub fn apply_form(&mut self, done: Submitted, fx: &mut Vec<Effect>) {
        let send = |fx: &mut Vec<Effect>, command: Command, show_response: bool| {
            let label = command.name().to_owned();
            fx.push(Effect::Send {
                label,
                command,
                show_response,
            });
        };
        match done {
            Submitted::Authorize { id_tag } => send(fx, Command::Authorize { id_tag }, false),
            Submitted::StartTx { connector, id_tag } => {
                send(fx, Command::StartTransaction { connector, id_tag }, false);
            }
            Submitted::Meter {
                connector,
                wh_delta,
                measurands,
            } => {
                send(
                    fx,
                    Command::MeterValues {
                        connector,
                        wh_delta,
                        measurands,
                    },
                    false,
                );
            }
            Submitted::Data {
                vendor_id,
                message_id,
                data,
            } => {
                send(
                    fx,
                    Command::DataTransfer {
                        vendor_id,
                        message_id,
                        data,
                    },
                    true,
                );
            }
            Submitted::Raw { action, payload } => {
                fx.push(Effect::Send {
                    label: format!("raw {action}"),
                    command: Command::SendRaw {
                        action,
                        payload,
                        validate: true,
                    },
                    show_response: true,
                });
            }
            Submitted::Rule(rule) => {
                let label = format!("rule {rule}");
                fx.push(Effect::Send {
                    label,
                    command: Command::SetResponseRule { rule },
                    show_response: false,
                });
            }
            Submitted::RuleOff(action) => {
                let label = format!("rule {action} off");
                fx.push(Effect::Send {
                    label,
                    command: Command::ClearResponseRules {
                        action: Some(action),
                    },
                    show_response: false,
                });
            }
            Submitted::ConfigValue { key, value } => {
                let label = format!("set {key}");
                fx.push(Effect::Send {
                    label,
                    command: Command::SetConfiguration { key, value },
                    show_response: false,
                });
            }
            Submitted::LocalEntry { id_tag, info } => {
                let label = format!("local list {id_tag}");
                fx.push(Effect::Send {
                    label,
                    command: Command::SetLocalListEntry { id_tag, info },
                    show_response: false,
                });
            }
            Submitted::Profile { connector, profile } => {
                let label = format!("install profile {}", profile.charging_profile_id);
                fx.push(Effect::Send {
                    label,
                    command: Command::InstallChargingProfile {
                        connector,
                        profile: *profile,
                    },
                    show_response: false,
                });
            }
            Submitted::Search(text) => {
                self.log.search = text;
                let n = self.log.visible_count();
                self.notify(NoticeLevel::Info, format!("{n} matching rows"));
            }
            Submitted::Export(path) => {
                let contents: String = self.log.all().map(|e| event_line(e) + "\n").collect();
                fx.push(Effect::WriteFile { path, contents });
            }
            Submitted::LoadScenario(path) => {
                self.scenario = ScenarioView {
                    name: path.clone(),
                    running: true,
                    ..ScenarioView::default()
                };
                fx.push(Effect::RunScenarioFile(path));
            }
        }
    }

    pub fn open_detail(&mut self) {
        let Some(entry) = self.log.selected() else {
            return;
        };
        let mut lines = vec![entry_header(entry)];
        lines.push(String::new());
        lines.extend(entry.pretty().lines().map(crate::text::sanitize));
        if let Some(peer) = entry.peer.and_then(|s| self.log.get(s)) {
            lines.push(String::new());
            lines.push(format!(
                "--- {} ---",
                if peer.seq > entry.seq {
                    "answer"
                } else {
                    "request"
                }
            ));
            lines.push(entry_header(peer));
            lines.extend(peer.pretty().lines().map(crate::text::sanitize));
        }
        let title = entry
            .frame()
            .and_then(|f| f.action.clone())
            .unwrap_or_else(|| "log line".to_owned());
        self.mode = Mode::Detail(Detail {
            title,
            copy: entry.pretty(),
            lines,
            scroll: 0,
        });
    }
}

fn entry_header(e: &LogEntry) -> String {
    let rtt = e
        .rtt_ms
        .map_or_else(String::new, |ms| format!("  ({ms} ms)"));
    match e.frame() {
        Some(f) => format!(
            "{}  {}  id={}{}",
            e.ts.format("%Y-%m-%d %H:%M:%S%.3f UTC"),
            match f.direction {
                kirby_ocpp_core::station::Direction::In => "CSMS -> station",
                kirby_ocpp_core::station::Direction::Out => "station -> CSMS",
            },
            f.msg_id.as_deref().unwrap_or("-"),
            rtt
        ),
        None => e.ts.format("%Y-%m-%d %H:%M:%S%.3f UTC").to_string(),
    }
}

/// One JSONL line for a log entry (the same shape the station events serialize to).
pub fn event_line(e: &LogEntry) -> String {
    serde_json::to_string(&e.event).unwrap_or_default()
}

/// Folds one message into the model and returns what the loop must do.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    let mut fx = Vec::new();
    match msg {
        Msg::Key(key) => crate::keys::handle(app, key, &mut fx),
        Msg::Paste(text) => match &mut app.mode {
            Mode::Form(f) => f.paste(&text),
            Mode::Palette(p) => {
                p.query.insert_str(&text);
                p.refresh(&app.catalog);
            }
            Mode::Browser(b) if b.filtering => {
                b.filter.insert_str(&text);
                b.reset_selection();
            }
            _ => {}
        },
        Msg::Resize(w, h) => app.size = (w, h),
        Msg::Tick => {
            let expired = match &mut app.notice {
                Some(n) => {
                    n.ttl = n.ttl.saturating_sub(1);
                    n.ttl == 0
                }
                None => false,
            };
            if expired {
                app.notice = None;
            } else {
                return fx;
            }
        }
        Msg::Snapshot(s) => {
            app.snapshot = *s;
            if app
                .snapshot
                .connector(app.connector)
                .is_none_or(|c| c.id.is_station())
                && let Some(first) = app.snapshot.connectors.first()
            {
                app.connector = first.id;
            }
        }
        Msg::Event(ev) => app.log.push(ev),
        Msg::Scenario(p) => apply_progress(&mut app.scenario, p),
        Msg::ScenarioDone(report) => {
            app.scenario.running = false;
            let (level, text) = if report.passed {
                (
                    NoticeLevel::Success,
                    format!("scenario {} passed", report.scenario),
                )
            } else if report.cancelled {
                (
                    NoticeLevel::Info,
                    format!("scenario {} cancelled", report.scenario),
                )
            } else {
                (
                    NoticeLevel::Error,
                    format!(
                        "scenario {} failed: {}",
                        report.scenario,
                        first_failure(&report)
                    ),
                )
            };
            for step in &report.steps {
                apply_progress(
                    &mut app.scenario,
                    ScenarioProgress::StepFinished(step.clone()),
                );
            }
            app.scenario.report = Some(*report);
            app.notify(level, text);
        }
        Msg::ScenarioError(e) => {
            app.scenario.running = false;
            app.notify(NoticeLevel::Error, e);
        }
        Msg::CommandDone {
            label,
            show_response,
            result,
        } => command_done(app, &label, show_response, result),
        Msg::Notice(level, text) => app.notify(level, text),
        Msg::Signal => app.quit = true,
    }
    app.dirty = true;
    fx
}

fn first_failure(r: &ScenarioReport) -> String {
    r.steps
        .iter()
        .find_map(|s| match &s.status {
            StepStatus::Failed(why) => Some(format!("step {} ({}): {why}", s.index + 1, s.name)),
            _ => None,
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn apply_progress(view: &mut ScenarioView, p: ScenarioProgress) {
    match p {
        ScenarioProgress::StepStarted { index, name } => {
            if view.steps.len() <= index {
                view.steps
                    .resize(index + 1, (String::new(), StepState::Pending));
            }
            view.steps[index] = (name, StepState::Running);
        }
        ScenarioProgress::StepFinished(StepReport {
            index,
            name,
            status,
            duration_ms,
        }) => {
            if view.steps.len() <= index {
                view.steps
                    .resize(index + 1, (String::new(), StepState::Pending));
            }
            view.steps[index] = (
                name,
                match status {
                    StepStatus::Passed => StepState::Passed(duration_ms),
                    StepStatus::Failed(why) => StepState::Failed(why),
                    StepStatus::Skipped => StepState::Skipped,
                },
            );
        }
    }
}

fn command_done(app: &mut App, label: &str, show_response: bool, result: Result<Outcome, String>) {
    match result {
        Err(e) => app.notify(NoticeLevel::Error, format!("{label}: {e}")),
        Ok(Outcome::Response { payload }) if show_response => {
            let pretty = serde_json::to_string_pretty(&payload).unwrap_or_default();
            app.mode = Mode::Detail(Detail {
                title: format!("Response to {label}"),
                lines: pretty.lines().map(crate::text::sanitize).collect(),
                scroll: 0,
                copy: pretty,
            });
        }
        Ok(Outcome::Authorized(d)) => {
            let level = if d.accepted {
                NoticeLevel::Success
            } else {
                NoticeLevel::Error
            };
            let verdict = if d.accepted { "accepted" } else { "rejected" };
            app.notify(level, format!("Authorize: {verdict}"));
        }
        Ok(Outcome::TransactionStarted { connector, tx_id }) => {
            app.notify(
                NoticeLevel::Success,
                format!("transaction {tx_id} started on connector {connector}"),
            );
        }
        Ok(Outcome::TransactionRejected { status }) => {
            app.notify(NoticeLevel::Error, format!("start rejected: {status}"));
        }
        Ok(Outcome::ConnectorAdded { connector }) => {
            app.notify(NoticeLevel::Success, format!("connector {connector} added"));
        }
        Ok(_) => app.notify(NoticeLevel::Success, format!("{label}: ok")),
    }
}

#[cfg(test)]
mod tests;

/// Charge point id as a safe file name part (ids may contain `/`, `..` or control characters).
fn export_name_part(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .replace("..", "_")
}

#[cfg(test)]
mod export_name_tests {
    use super::export_name_part;

    #[test]
    fn export_file_name_cannot_escape_the_directory() {
        assert_eq!(
            export_name_part("../../etc/CP 1"),
            "__-_etc_CP_1".replace("__-", "___")
        );
        assert!(!export_name_part("a/b\\c\n").contains(['/', '\\', '\n']));
    }
}
