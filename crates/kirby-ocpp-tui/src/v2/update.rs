//! The pure update function of the OCPP 2.x model.

use std::time::Instant;

use kirby_ocpp_core::scenario::{
    Scenario, ScenarioProgress, ScenarioReport, StepReport, StepStatus,
};
use kirby_ocpp_core::station::{Direction, Event};
use kirby_ocpp_core::station2::{Command2, Outcome2};
use kirby_ocpp_core::ws::ConnectionState;
use serde_json::Value;

use super::actions::ActionId2;
use super::app::{App2, Confirm2, ConfirmAction2, Effect2, Mode2, Msg2};
use super::browser::DeviceBrowser;
use super::forms::{
    authorize_form, charging_state_choice, data_transfer_form, meter_custom_form,
    present_token_form, raw_call_form, security_event_form, stop_reason_choice,
};
use super::keys;
use crate::app::{Detail, Notice, NoticeLevel, PendingCall, ScenarioView, StepState, event_line};
use crate::forms::Form;
use crate::logview::LogEntry;

/// Applies one message to the model and returns what the event loop must do.
pub fn update2(app: &mut App2, msg: Msg2) -> Vec<Effect2> {
    let mut fx = Vec::new();
    match msg {
        Msg2::Key(key) => keys::handle_key(app, key, &mut fx),
        Msg2::Paste(text) => match &mut app.mode {
            Mode2::Form(f) => f.paste(&text),
            Mode2::Palette(p) => {
                p.query.insert_str(&text);
                p.refresh_rows(&super::actions::palette_rows(&app.catalog));
            }
            Mode2::DeviceModel(b) if b.filtering => {
                b.query.insert_str(&text);
                b.selected = 0;
            }
            _ => {}
        },
        Msg2::Resize(w, h) => app.size = (w, h),
        Msg2::Tick => {
            // The elapsed times show whole seconds: redraw only when one of them changed.
            let now = Instant::now();
            let changed = app.pending.iter().any(|p| {
                now.saturating_duration_since(p.since).as_secs()
                    != app.now.saturating_duration_since(p.since).as_secs()
            });
            app.now = now;
            if changed {
                app.dirty = true;
            }
            let expired = app.notice.as_mut().is_some_and(Notice::tick);
            if expired {
                app.notice = None;
            } else {
                return fx;
            }
        }
        Msg2::Snapshot(s) => {
            // Replies cannot arrive over a connection that is gone: drop the "waiting" marker.
            if app.snapshot.connection == ConnectionState::Connected
                && s.connection != ConnectionState::Connected
            {
                app.pending.clear();
            }
            app.snapshot = *s;
            if app.snapshot.evse(app.evse).is_none() {
                app.evse = app.snapshot.evses.first().map_or(0, |e| e.id);
            }
        }
        Msg2::Event(ev) => {
            count_security_event(app, &ev);
            app.log.push(ev);
        }
        Msg2::Scenario(p) => apply_progress(&mut app.scenario, *p),
        Msg2::ScenarioDone(report) => scenario_done(app, *report),
        Msg2::ScenarioError(e) => {
            app.scenario.running = false;
            app.notify(NoticeLevel::Error, e);
        }
        Msg2::CommandDone {
            label,
            show_response,
            result,
        } => {
            if let Some(i) = app.pending.iter().position(|p| p.label == label) {
                app.pending.remove(i);
            }
            if let Mode2::Pending(sel) = &mut app.mode {
                *sel = (*sel).min(app.pending.len().saturating_sub(1));
            }
            command_done(app, &label, show_response, result);
        }
        Msg2::Notice(level, text) => app.notify(level, text),
        Msg2::Signal => app.quit = true,
    }
    for e in &fx {
        if let Effect2::Send {
            label, commands, ..
        } = e
        {
            app.pending.push(PendingCall {
                label: label.clone(),
                action: commands.iter().find_map(|command| match command {
                    Command2::Boot => Some("BootNotification".to_owned()),
                    Command2::Heartbeat => Some("Heartbeat".to_owned()),
                    Command2::Authorize { .. } | Command2::PresentToken { .. } => {
                        Some("Authorize".to_owned())
                    }
                    Command2::StatusNotification { .. } => Some("StatusNotification".to_owned()),
                    Command2::SendSecurityEvent { .. } => {
                        Some("SecurityEventNotification".to_owned())
                    }
                    Command2::DataTransfer { .. } => Some("DataTransfer".to_owned()),
                    Command2::SendRaw { action, .. } => Some(action.clone()),
                    _ => None,
                }),
                since: Instant::now(),
            });
        }
    }
    app.dirty = true;
    fx
}

/// Counts an outbound `SecurityEventNotification` CALL (the snapshot has no list of them).
fn count_security_event(app: &mut App2, ev: &Event) {
    if let Event::Frame(f) = ev
        && f.direction == Direction::Out
        && f.frame.get(0).and_then(Value::as_u64) == Some(2)
        && f.frame.get(2).and_then(Value::as_str) == Some("SecurityEventNotification")
    {
        app.security_events_sent += 1;
    }
}

fn scenario_done(app: &mut App2, report: ScenarioReport) {
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
    app.scenario.report = Some(report);
    app.notify(level, text);
}

fn first_failure(r: &ScenarioReport) -> String {
    r.steps
        .iter()
        .find_map(|s| match &s.status {
            StepStatus::Failed(why) => Some(format!(
                "step {} ({}): {}",
                s.index + 1,
                s.name,
                crate::text::without_step_prefix(&s.name, why)
            )),
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
            ..
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

fn command_done(
    app: &mut App2,
    label: &str,
    show_response: bool,
    result: Result<Outcome2, String>,
) {
    match result {
        Err(e) => app.notify(NoticeLevel::Error, format!("{label}: {e}")),
        Ok(Outcome2::Response { payload }) if show_response => {
            let pretty = serde_json::to_string_pretty(&payload).unwrap_or_default();
            app.mode = Mode2::Detail(Detail {
                title: format!("Response to {label}"),
                lines: pretty.lines().map(crate::text::sanitize).collect(),
                scroll: 0,
                copy: pretty,
            });
        }
        Ok(Outcome2::Authorized { status, .. }) => {
            let level = if status == "Accepted" {
                NoticeLevel::Success
            } else {
                NoticeLevel::Error
            };
            app.notify(level, format!("{label}: {status}"));
        }
        Ok(Outcome2::TransactionStarted {
            evse,
            transaction_id,
        }) => app.notify(
            NoticeLevel::Success,
            format!("transaction {transaction_id} started on EVSE {evse}"),
        ),
        Ok(_) => app.notify(NoticeLevel::Success, format!("{label}: ok")),
    }
}

impl App2 {
    /// Runs an action, or explains why it cannot run.
    pub fn run_action2(&mut self, id: ActionId2, fx: &mut Vec<Effect2>) {
        if let Some(reason) = self.reason_for2(id) {
            self.notify(NoticeLevel::Error, reason);
            return;
        }
        let evse = self.evse;
        let max_evse = self.snapshot.evses.last().map_or(1, |e| e.id);
        let send = |fx: &mut Vec<Effect2>, label: &str, command: Command2, show: bool| {
            fx.push(Effect2::Send {
                label: label.to_owned(),
                commands: vec![command],
                show_response: show,
            });
        };
        let plain = |fx: &mut Vec<Effect2>, command: Command2| {
            let label = command.name().replace('_', " ");
            send(fx, &label, command, false);
        };
        match id {
            ActionId2::Boot => plain(fx, Command2::Boot),
            ActionId2::Heartbeat => plain(fx, Command2::Heartbeat),
            ActionId2::Reconnect => plain(fx, Command2::Reconnect),
            ActionId2::Disconnect => plain(fx, Command2::Disconnect),
            ActionId2::Connect => plain(fx, Command2::Connect),
            ActionId2::DeviceModel => self.mode = Mode2::DeviceModel(DeviceBrowser::new()),
            ActionId2::SecurityEvent => self.mode = Mode2::Form(security_event_form()),
            ActionId2::PendingCalls => self.mode = Mode2::Pending(0),
            ActionId2::PlugToggle => {
                let plugged = self
                    .selected_evse()
                    .is_some_and(|e| e.connectors.iter().any(|c| c.ev_connected));
                plain(
                    fx,
                    if plugged {
                        Command2::Unplug { evse }
                    } else {
                        Command2::PlugIn { evse }
                    },
                );
            }
            ActionId2::FaultToggle => {
                let faulted = self
                    .selected_evse()
                    .is_some_and(|e| e.connectors.iter().any(|c| c.fault));
                plain(
                    fx,
                    if faulted {
                        Command2::ClearFault {
                            evse,
                            connector: None,
                        }
                    } else {
                        Command2::Fault {
                            evse,
                            connector: None,
                        }
                    },
                );
            }
            ActionId2::StatusNotification => {
                let connector = self
                    .selected_evse()
                    .and_then(|e| e.connectors.first())
                    .map_or(1, |c| c.id);
                plain(fx, Command2::StatusNotification { evse, connector });
            }
            ActionId2::Authorize => self.mode = Mode2::Form(authorize_form(self.protocol)),
            ActionId2::PresentToken => {
                self.mode = Mode2::Form(present_token_form(evse, self.protocol));
            }
            ActionId2::StopTransaction => {
                self.mode = Mode2::Choice(stop_reason_choice(evse, self.protocol));
            }
            ActionId2::Meter120 => fx.push(Effect2::Send {
                label: "meter +120 Wh".to_owned(),
                commands: vec![
                    Command2::AdvanceMeter {
                        evse,
                        wh_delta: 120,
                    },
                    Command2::SampleMeter { evse },
                ],
                show_response: false,
            }),
            ActionId2::MeterCustom => {
                self.mode = Mode2::Form(meter_custom_form(evse, max_evse));
            }
            ActionId2::ChargingState => self.mode = Mode2::Choice(charging_state_choice(evse)),
            ActionId2::DataTransfer => self.mode = Mode2::Form(data_transfer_form(&self.vendor)),
            ActionId2::VendorPing => send(
                fx,
                "vendor ping",
                Command2::DataTransfer {
                    vendor_id: self.vendor.clone(),
                    message_id: None,
                    data: None,
                },
                true,
            ),
            ActionId2::RawCall => self.mode = Mode2::Form(raw_call_form(self.protocol)),
            ActionId2::LogCopySelected => {
                if let Some(e) = self.log.selected() {
                    fx.push(Effect2::Copy {
                        text: e.pretty(),
                        what: "selected row".to_owned(),
                    });
                }
            }
            ActionId2::LogCopyVisible => {
                let text: Vec<String> = self.log.visible().iter().map(|e| event_line(e)).collect();
                fx.push(Effect2::Copy {
                    text: text.join("\n"),
                    what: format!("{} rows", text.len()),
                });
            }
            ActionId2::LogFilter => {
                let next = self.log.filter.next();
                self.log.set_filter(next);
                self.notify(NoticeLevel::Info, format!("log filter: {}", next.label()));
            }
            ActionId2::LogSearch => self.mode = Mode2::Form(Form::search(&self.log.search)),
            ActionId2::LogPause => {
                self.log.toggle_follow();
                let state = if self.log.follow {
                    "following"
                } else {
                    "paused"
                };
                self.notify(NoticeLevel::Info, format!("log {state}"));
            }
            ActionId2::LogBottom => self.log.follow_tail(),
            ActionId2::LogExport => {
                let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
                let path = format!(
                    "./kirby-ocpp-{}-{stamp}.jsonl",
                    file_name_part(&self.snapshot.charge_point_id)
                );
                self.mode = Mode2::Form(Form::export(&path));
            }
            ActionId2::LogClear => {
                self.mode = Mode2::Confirm(Confirm2 {
                    message: format!("Clear all {} log rows?", self.log.len()),
                    action: ConfirmAction2::ClearLog,
                });
            }
            ActionId2::RunScenario(i) => {
                if let Some(s) = self.scenarios.get(i).cloned() {
                    self.start_scenario(s, fx);
                }
            }
            ActionId2::LoadScenario => self.mode = Mode2::Form(Form::load_scenario()),
            ActionId2::CancelScenario => {
                fx.push(Effect2::CancelScenario);
                self.notify(NoticeLevel::Info, "cancelling scenario...");
            }
        }
    }

    fn start_scenario(&mut self, s: Scenario, fx: &mut Vec<Effect2>) {
        self.scenario = ScenarioView {
            name: s.name.clone(),
            steps: s
                .expanded_steps()
                .into_iter()
                .map(|st| (st.name, StepState::Pending))
                .collect(),
            running: true,
            report: None,
        };
        self.notify(NoticeLevel::Info, format!("running scenario {}", s.name));
        fx.push(Effect2::RunScenario(Box::new(s)));
    }

    /// Opens the detail view of the selected log row.
    pub fn open_detail2(&mut self) {
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
        self.mode = Mode2::Detail(Detail {
            title: crate::text::sanitize(&title),
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
                Direction::In => "CSMS -> station",
                Direction::Out => "station -> CSMS",
            },
            f.msg_id.as_deref().unwrap_or("-"),
            rtt
        ),
        None => e.ts.format("%Y-%m-%d %H:%M:%S%.3f UTC").to_string(),
    }
}

/// Charge point id as a safe file name part (ids may contain `/`, `..` or control characters).
fn file_name_part(id: &str) -> String {
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
