//! The catalogue of actions (palette and hotkeys), when each is available, and what the model
//! does when one runs, is confirmed, or finishes.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use crossterm::event::KeyCode;
use kirby_csms_core::registry;
use kirby_csms_core::{ACTIONS_PROTOCOL, Frame, TransactionStatus};
use kirby_ocpp_core::ocpp::redact_secrets;
use kirby_ocpp_tui::app::{Detail, NoticeLevel};
use kirby_ocpp_tui::palette::rank_by;
use kirby_ocpp_tui::text::{TextInput, sanitize, truncate};
use serde_json::Value;

use crate::app::{App, Confirm, ConfirmAction, Effect, Mode};
use crate::command::{CmdOutcome, Command};
use crate::form::{Form, StopChoice, Submitted};

/// Results remembered per station.
pub const RESULTS_PER_STATION: usize = 20;
/// Lines of one result kept (an answer can be large).
const RESULT_LINES: usize = 2000;
/// Longest text of a status in a notice.
const NOTICE_TEXT: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    RemoteStart,
    RemoteStop,
    Reset,
    ChangeConfiguration,
    GetConfiguration,
    Trigger,
    Unlock,
    Availability,
    ClearCache,
    SetProfile,
    DataTransfer,
    GetLog,
    InstallCertificate,
    DeleteCertificate,
    CertificateIds,
    ExtendedTrigger,
    SignedFirmware,
    CertificateSigned,
    ClearChargingProfile,
    GetCompositeSchedule,
    ReserveNow,
    UpdateFirmware,
    GetDiagnostics,
    SendLocalList,
    SecurityEvents,
    Raw,
    Rule,
    RemoveRule,
    Fault,
    ClearRules,
    Forget,
    CopyId,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionDef {
    pub id: ActionId,
    label: &'static str,
    desc: &'static str,
    /// The hotkey in the normal mode, if any.
    pub key: Option<char>,
    /// The OCPP action this entry sends. Its title and description come from the action
    /// registry of [`ACTIONS_PROTOCOL`]; `label` and `desc` are used only when it is `None`.
    pub ocpp: Option<&'static str>,
}

impl ActionDef {
    /// The palette title.
    pub fn label(&self) -> &'static str {
        self.spec().map_or(self.label, |spec| spec.title)
    }

    /// The one-line palette description.
    pub fn desc(&self) -> &'static str {
        self.spec().map_or(self.desc, |spec| spec.summary)
    }

    /// The registry entry of the OCPP action behind this entry.
    pub fn spec(&self) -> Option<&'static registry::ActionSpec> {
        registry::csms_action(ACTIONS_PROTOCOL, self.ocpp?)
    }
}

/// An entry that is not an OCPP action (a local operation).
const fn def(
    id: ActionId,
    label: &'static str,
    desc: &'static str,
    key: Option<char>,
) -> ActionDef {
    ActionDef {
        id,
        label,
        desc,
        key,
        ocpp: None,
    }
}

/// An entry that sends the OCPP action `name`; the registry supplies the texts.
const fn ocpp(id: ActionId, name: &'static str, key: Option<char>) -> ActionDef {
    ActionDef {
        id,
        label: "",
        desc: "",
        key,
        ocpp: Some(name),
    }
}

/// Every action, in palette order.
pub const CATALOG: &[ActionDef] = &[
    ocpp(ActionId::RemoteStart, "RemoteStartTransaction", Some('t')),
    ocpp(ActionId::RemoteStop, "RemoteStopTransaction", Some('x')),
    ocpp(ActionId::Reset, "Reset", Some('R')),
    ocpp(
        ActionId::ChangeConfiguration,
        "ChangeConfiguration",
        Some('c'),
    ),
    ocpp(ActionId::GetConfiguration, "GetConfiguration", Some('g')),
    ocpp(ActionId::Trigger, "TriggerMessage", Some('T')),
    ocpp(ActionId::Unlock, "UnlockConnector", Some('u')),
    ocpp(ActionId::Availability, "ChangeAvailability", Some('A')),
    ocpp(ActionId::ClearCache, "ClearCache", Some('C')),
    ocpp(ActionId::SetProfile, "SetChargingProfile", Some('P')),
    ocpp(ActionId::DataTransfer, "DataTransfer", Some('D')),
    ocpp(ActionId::GetLog, "GetLog", Some('l')),
    ocpp(
        ActionId::InstallCertificate,
        "InstallCertificate",
        Some('I'),
    ),
    ocpp(ActionId::DeleteCertificate, "DeleteCertificate", Some('d')),
    ocpp(
        ActionId::CertificateIds,
        "GetInstalledCertificateIds",
        Some('L'),
    ),
    ocpp(
        ActionId::ExtendedTrigger,
        "ExtendedTriggerMessage",
        Some('E'),
    ),
    ocpp(ActionId::SignedFirmware, "SignedUpdateFirmware", Some('S')),
    ocpp(ActionId::CertificateSigned, "CertificateSigned", Some('X')),
    ocpp(
        ActionId::ClearChargingProfile,
        "ClearChargingProfile",
        Some('K'),
    ),
    ocpp(
        ActionId::GetCompositeSchedule,
        "GetCompositeSchedule",
        Some('H'),
    ),
    ocpp(ActionId::ReserveNow, "ReserveNow", Some('V')),
    ocpp(ActionId::UpdateFirmware, "UpdateFirmware", Some('U')),
    ocpp(ActionId::GetDiagnostics, "GetDiagnostics", Some('O')),
    ocpp(ActionId::SendLocalList, "SendLocalList", Some('B')),
    def(
        ActionId::SecurityEvents,
        "Security events",
        "the last security events the selected station reported",
        Some('e'),
    ),
    def(
        ActionId::Raw,
        "Raw call",
        "any central system action with a JSON payload",
        Some('w'),
    ),
    def(
        ActionId::Rule,
        "Response rule",
        "answer every station's action with a fixed payload",
        Some('m'),
    ),
    def(
        ActionId::RemoveRule,
        "Remove response rule",
        "stop overriding the answer to one action",
        None,
    ),
    def(
        ActionId::Fault,
        "Inject fault",
        "CALLERROR, silence, close or invalid payload for the next calls",
        Some('i'),
    ),
    def(
        ActionId::ClearRules,
        "Clear all rules and faults",
        "remove every response rule and pending fault",
        Some('M'),
    ),
    def(
        ActionId::Forget,
        "Forget station",
        "drop a disconnected station and its state (asks first)",
        Some('F'),
    ),
    def(
        ActionId::CopyId,
        "Copy station id",
        "copy the id of the selected station",
        Some('n'),
    ),
    def(
        ActionId::Results,
        "Command results",
        "the last answers from the selected station",
        Some('r'),
    ),
];

/// The action a normal-mode key triggers.
pub fn by_key(c: char) -> Option<ActionId> {
    CATALOG.iter().find(|a| a.key == Some(c)).map(|a| a.id)
}

pub fn def_of(id: ActionId) -> Option<&'static ActionDef> {
    CATALOG.iter().find(|a| a.id == id)
}

/// Text the palette matches on.
fn haystack(a: &ActionDef) -> String {
    format!("{} {}", a.label(), a.desc())
}

/// Command palette: fuzzy search over [`CATALOG`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub query: TextInput,
    /// Indices into [`CATALOG`], best match first.
    pub results: Vec<usize>,
    pub selected: usize,
}

impl Palette {
    pub fn new() -> Self {
        let mut p = Self {
            query: TextInput::default(),
            results: Vec::new(),
            selected: 0,
        };
        p.refresh();
        p
    }

    pub fn refresh(&mut self) {
        self.results = rank_by(CATALOG, self.query.text(), haystack);
        self.selected = 0;
    }

    pub fn move_by(&mut self, d: isize) {
        if !self.results.is_empty() {
            let n = self.results.len();
            self.selected = (self.selected + n).wrapping_add_signed(d) % n;
        }
    }

    pub fn chosen(&self) -> Option<ActionId> {
        self.results
            .get(self.selected)
            .and_then(|i| CATALOG.get(*i))
            .map(|a| a.id)
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::new()
    }
}

/// The list of a station's last results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultsList {
    pub station: String,
    /// Row, newest first.
    pub selected: usize,
}

/// One finished command, rendered once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultRecord {
    pub at: DateTime<Utc>,
    pub label: String,
    pub ok: bool,
    pub summary: String,
    pub lines: Vec<String>,
    /// What `c` copies in the popup: the answer as pretty JSON, or the error text.
    pub copy: String,
}

/// Active server-side scripting, as the engine reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    /// Actions that have a global response rule, sorted.
    pub actions: Vec<String>,
    /// Rules of one station, or of one protocol, sorted by station then action.
    pub scoped: Vec<ScopedRule>,
    /// Fault occurrences still waiting, over all scopes.
    pub faults: usize,
}

/// A response rule that does not answer every station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedRule {
    /// The station it answers; `None` for a global rule limited to a protocol.
    pub station: Option<String>,
    pub action: String,
}

impl ScopedRule {
    /// `Action@station` (or the plain action for a protocol-limited global rule).
    pub fn label(&self) -> String {
        match &self.station {
            Some(station) => format!("{}@{station}", self.action),
            None => self.action.clone(),
        }
    }
}

impl Rules {
    pub fn is_empty(&self) -> bool {
        self.no_rules() && self.faults == 0
    }

    /// No response rule of any scope.
    pub fn no_rules(&self) -> bool {
        self.actions.is_empty() && self.scoped.is_empty()
    }
}

/// `action` for a global rule or fault, `action on <station>` for a station's.
fn scope_text(action: &str, station: Option<&str>) -> String {
    match station {
        Some(station) => format!("{action} on {}", sanitize(station)),
        None => action.to_owned(),
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// `key = value` lines of a `GetConfiguration` answer, readonly keys marked.
fn configuration_lines(answer: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let keys = answer
        .get("configurationKey")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    out.push(format!("{} keys", keys.len()));
    for entry in keys {
        let key = entry.get("key").and_then(Value::as_str).unwrap_or("?");
        let value = entry
            .get("value")
            .and_then(Value::as_str)
            .map_or_else(|| "(not set)".to_owned(), |v| format!("{v:?}"));
        let ro = entry
            .get("readonly")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        out.push(format!(
            "  {key} = {value}{}",
            if ro { "  (readonly)" } else { "" }
        ));
    }
    if let Some(unknown) = answer.get("unknownKey").and_then(Value::as_array)
        && !unknown.is_empty()
    {
        out.push("unknown keys:".to_owned());
        for k in unknown {
            out.push(format!("  {}", k.as_str().unwrap_or("?")));
        }
    }
    out
}

/// Readable lines of a `GetInstalledCertificateIds` answer.
fn certificate_lines(answer: &Value) -> Vec<String> {
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("?").to_owned();
    let entries = answer
        .get("certificateHashData")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut out = vec![format!("{} certificates", entries.len())];
    for (i, entry) in entries.iter().enumerate() {
        out.push(format!(
            "{}. {}  serial {}",
            i + 1,
            text(entry, "hashAlgorithm"),
            text(entry, "serialNumber")
        ));
        out.push(format!(
            "     issuerNameHash {}",
            text(entry, "issuerNameHash")
        ));
        out.push(format!(
            "     issuerKeyHash  {}",
            text(entry, "issuerKeyHash")
        ));
    }
    out
}

/// The scrollable list of the security events a station reported, newest first.
fn security_events_detail(station: &str, state: &kirby_csms_core::StationState) -> Detail {
    let mut lines = vec![format!(
        "{} reported, the last {} are kept (newest first)",
        state.security_events_total,
        state.security_events.len()
    )];
    for e in state.security_events.iter().rev() {
        let info = e
            .tech_info
            .as_deref()
            .map_or_else(String::new, |t| format!("  {t}"));
        lines.push(sanitize(&format!(
            "{}  {}{info}",
            e.timestamp.format("%Y-%m-%d %H:%M:%S"),
            e.event_type
        )));
    }
    let copy = lines.join("\n");
    Detail {
        title: format!(
            "Security events of {}",
            truncate(&sanitize(station), NOTICE_TEXT)
        ),
        lines,
        scroll: 0,
        copy,
    }
}

/// Statuses that mean the station refused.
fn refused(status: &str) -> bool {
    matches!(
        status,
        "Rejected"
            | "NotSupported"
            | "UnlockFailed"
            | "Failed"
            | "Unknown"
            | "NotImplemented"
            | "NotFound"
            | "InvalidCertificate"
            | "RevokedCertificate"
    )
}

impl App {
    fn summary(&self, id: &str) -> Option<&crate::app::Summary> {
        let i = self
            .stations
            .binary_search_by(|s| s.id.as_str().cmp(id))
            .ok()?;
        self.stations.get(i)
    }

    fn active_transactions(&self) -> Vec<StopChoice> {
        self.state
            .as_ref()
            .map(|s| {
                s.transactions
                    .iter()
                    .rev()
                    .filter(|t| t.status == TransactionStatus::Active)
                    .map(|t| StopChoice {
                        id: t.id,
                        connector: t.connector,
                        id_tag: t.id_tag.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Why `id` cannot run now, or `None` when it can.
    pub fn reason_for(&self, id: ActionId) -> Option<String> {
        let reason = |s: &str| Some(s.to_owned());
        let Some(selected) = self.selected.as_deref() else {
            return match id {
                ActionId::Rule | ActionId::Fault => None,
                ActionId::RemoveRule => self
                    .rules
                    .no_rules()
                    .then(|| "no response rules".to_owned()),
                ActionId::ClearRules => self
                    .rules
                    .is_empty()
                    .then(|| "no rules or faults".to_owned()),
                _ => reason("no station selected"),
            };
        };
        let connected = self.summary(selected).is_some_and(|s| s.connected);
        match id {
            ActionId::RemoteStart
            | ActionId::Reset
            | ActionId::ChangeConfiguration
            | ActionId::GetConfiguration
            | ActionId::Trigger
            | ActionId::Unlock
            | ActionId::Availability
            | ActionId::ClearCache
            | ActionId::SetProfile
            | ActionId::DataTransfer
            | ActionId::GetLog
            | ActionId::InstallCertificate
            | ActionId::DeleteCertificate
            | ActionId::CertificateIds
            | ActionId::ExtendedTrigger
            | ActionId::SignedFirmware
            | ActionId::CertificateSigned
            | ActionId::ClearChargingProfile
            | ActionId::GetCompositeSchedule
            | ActionId::ReserveNow
            | ActionId::UpdateFirmware
            | ActionId::GetDiagnostics
            | ActionId::SendLocalList
            | ActionId::Raw => (!connected).then(|| "station is disconnected".to_owned()),
            ActionId::RemoteStop => {
                if !connected {
                    reason("station is disconnected")
                } else if self.active_transactions().is_empty() {
                    reason("no active transaction")
                } else {
                    None
                }
            }
            ActionId::SecurityEvents => self
                .state
                .as_ref()
                .is_none_or(|s| s.security_events.is_empty())
                .then(|| "no security events".to_owned()),
            ActionId::Rule | ActionId::Fault | ActionId::CopyId => None,
            ActionId::RemoveRule => self
                .rules
                .no_rules()
                .then(|| "no response rules".to_owned()),
            ActionId::ClearRules => self
                .rules
                .is_empty()
                .then(|| "no rules or faults".to_owned()),
            ActionId::Forget => connected.then(|| "station is connected".to_owned()),
            ActionId::Results => self
                .results
                .get(selected)
                .is_none_or(VecDeque::is_empty)
                .then(|| "no results yet".to_owned()),
        }
    }

    pub fn open_palette(&mut self) {
        self.mode = Mode::Palette(Palette::new());
    }

    /// Runs an action from a key or the palette; an unavailable one says why.
    pub fn run_action(&mut self, id: ActionId, fx: &mut Vec<Effect>) {
        if let Some(why) = self.reason_for(id) {
            let label = def_of(id).map_or("action", ActionDef::label);
            self.notify(NoticeLevel::Error, format!("{label}: {why}"));
            return;
        }
        let station = self.selected.clone().unwrap_or_default();
        let st = station.as_str();
        match id {
            ActionId::RemoteStart => self.mode = Mode::Form(Box::new(Form::remote_start(st))),
            ActionId::RemoteStop => {
                let choices = self.active_transactions();
                self.mode = Mode::Form(Box::new(Form::remote_stop(st, &choices)));
            }
            ActionId::Reset => self.mode = Mode::Form(Box::new(Form::reset(st))),
            ActionId::ChangeConfiguration => {
                self.mode = Mode::Form(Box::new(Form::change_configuration(st)));
            }
            ActionId::GetConfiguration => {
                self.mode = Mode::Form(Box::new(Form::get_configuration(st)));
            }
            ActionId::Trigger => self.mode = Mode::Form(Box::new(Form::trigger(st))),
            ActionId::Unlock => {
                let first = self
                    .state
                    .as_ref()
                    .and_then(|s| s.connectors.keys().copied().find(|k| *k >= 1));
                self.mode = Mode::Form(Box::new(Form::unlock(st, first)));
            }
            ActionId::Availability => self.mode = Mode::Form(Box::new(Form::availability(st))),
            ActionId::ClearCache => self.send(st, Command::ClearCache, fx),
            ActionId::SetProfile => self.mode = Mode::Form(Box::new(Form::set_profile(st))),
            ActionId::DataTransfer => self.mode = Mode::Form(Box::new(Form::data_transfer(st))),
            ActionId::GetLog => {
                let request_id = self.take_request_id();
                self.mode = Mode::Form(Box::new(Form::get_log(st, request_id)));
            }
            ActionId::InstallCertificate => {
                self.mode = Mode::Form(Box::new(Form::install_certificate(st)));
            }
            ActionId::DeleteCertificate => {
                self.mode = Mode::Form(Box::new(Form::delete_certificate(st)));
            }
            ActionId::CertificateIds => {
                self.mode = Mode::Form(Box::new(Form::get_installed_certificate_ids(st)));
            }
            ActionId::ExtendedTrigger => {
                self.mode = Mode::Form(Box::new(Form::extended_trigger(st)));
            }
            ActionId::SignedFirmware => {
                let request_id = self.take_request_id();
                self.mode = Mode::Form(Box::new(Form::signed_update_firmware(
                    st, request_id, self.now,
                )));
            }
            ActionId::CertificateSigned => {
                self.mode = Mode::Form(Box::new(Form::certificate_signed(st)));
            }
            ActionId::ClearChargingProfile => {
                self.mode = Mode::Form(Box::new(Form::clear_charging_profile(st)));
            }
            ActionId::GetCompositeSchedule => {
                self.mode = Mode::Form(Box::new(Form::get_composite_schedule(st)));
            }
            ActionId::ReserveNow => {
                let reservation_id = self.take_request_id();
                self.mode = Mode::Form(Box::new(Form::reserve_now(st, reservation_id, self.now)));
            }
            ActionId::UpdateFirmware => {
                self.mode = Mode::Form(Box::new(Form::update_firmware(st, self.now)));
            }
            ActionId::GetDiagnostics => {
                self.mode = Mode::Form(Box::new(Form::get_diagnostics(st)));
            }
            ActionId::SendLocalList => self.mode = Mode::Form(Box::new(Form::send_local_list(st))),
            ActionId::SecurityEvents => {
                if let Some(state) = &self.state {
                    self.mode = Mode::Detail(security_events_detail(&station, state));
                }
            }
            ActionId::Raw => self.mode = Mode::Form(Box::new(Form::raw(st, None))),
            ActionId::Rule => self.mode = Mode::Form(Box::new(Form::rule())),
            ActionId::RemoveRule => {
                self.mode = Mode::Form(Box::new(Form::remove_rule_scoped(
                    &self.rules.actions,
                    &self.rules.scoped,
                )));
            }
            ActionId::Fault => self.mode = Mode::Form(Box::new(Form::fault())),
            ActionId::ClearRules => {
                self.mode = Mode::Confirm(Confirm {
                    message: "Remove every response rule and pending fault?".to_owned(),
                    action: ConfirmAction::ClearRules,
                });
            }
            ActionId::Forget => {
                self.mode = Mode::Confirm(Confirm {
                    message: format!("Forget station {} and its state?", sanitize(st)),
                    action: ConfirmAction::Forget(station.clone()),
                });
            }
            ActionId::CopyId => fx.push(Effect::Copy {
                text: station.clone(),
                what: "station id".to_owned(),
            }),
            ActionId::Results => {
                self.mode = Mode::Results(ResultsList {
                    station: station.clone(),
                    selected: 0,
                });
            }
        }
    }

    /// A form was submitted successfully.
    pub fn submitted(&mut self, form_station: &str, submitted: Submitted, fx: &mut Vec<Effect>) {
        match submitted {
            Submitted::Send(command) => {
                if command.needs_confirm() {
                    self.mode = Mode::Confirm(Confirm {
                        message: format!("Send {} to {}?", command.label(), sanitize(form_station)),
                        action: ConfirmAction::Send {
                            station: form_station.to_owned(),
                            command,
                        },
                    });
                } else {
                    self.send(form_station, command, fx);
                }
            }
            Submitted::Rule {
                action,
                payload,
                station,
            } => {
                let target = scope_text(&action, station.as_deref());
                fx.push(Effect::Respond {
                    action,
                    payload,
                    station,
                });
                self.notify(NoticeLevel::Success, format!("rule set for {target}"));
            }
            Submitted::Fault {
                action,
                kind,
                times,
                payload,
                station,
            } => {
                let target = scope_text(&action, station.as_deref());
                fx.push(Effect::Inject {
                    action,
                    kind,
                    times,
                    payload,
                    station,
                });
                self.notify(
                    NoticeLevel::Success,
                    format!("fault {} x{times} for {target}", kind.name()),
                );
            }
            Submitted::RemoveRule(action) => {
                fx.push(Effect::RemoveRule(action.clone()));
                self.notify(NoticeLevel::Success, format!("rule removed for {action}"));
            }
            Submitted::RemoveScopedRule { station, action } => {
                let target = scope_text(&action, station.as_deref());
                fx.push(Effect::RemoveScopedRule { station, action });
                self.notify(NoticeLevel::Success, format!("rule removed for {target}"));
            }
        }
    }

    pub fn confirmed(&mut self, action: ConfirmAction, fx: &mut Vec<Effect>) {
        match action {
            ConfirmAction::Send { station, command } => self.send(&station, command, fx),
            ConfirmAction::Forget(station) => {
                fx.push(Effect::Forget(station));
            }
            ConfirmAction::ClearRules => {
                fx.push(Effect::ClearRules);
                self.notify(NoticeLevel::Success, "rules and faults cleared");
            }
        }
    }

    fn send(&mut self, station: &str, command: Command, fx: &mut Vec<Effect>) {
        self.pending += 1;
        self.notify(
            NoticeLevel::Info,
            format!(
                "sending {} to {}...",
                command.label(),
                truncate(&sanitize(station), NOTICE_TEXT)
            ),
        );
        fx.push(Effect::Send {
            station: station.to_owned(),
            command,
        });
    }

    /// A command finished: remember it, say so, and open the popup for answers with content.
    pub fn command_done(&mut self, station: &str, command: &Command, outcome: &CmdOutcome) {
        self.pending = self.pending.saturating_sub(1);
        let label = command.label();
        let who = truncate(&sanitize(station), NOTICE_TEXT);
        // The popup is a log of what was sent: never show an AuthorizationKey in it.
        let request = pretty(&redacted_payload(command));
        let mut lines = vec![
            format!(
                "{}  {}  {}",
                self.now.format("%H:%M:%S"),
                sanitize(station),
                label
            ),
            String::new(),
            "--- request ---".to_owned(),
        ];
        lines.extend(request.lines().map(sanitize));
        let (ok, summary, level, copy, content) = match &outcome.result {
            Ok(answer) => {
                let status = answer.get("status").and_then(Value::as_str);
                let text = status.map_or_else(|| "ok".to_owned(), sanitize);
                let level = if status.is_some_and(refused) {
                    NoticeLevel::Error
                } else {
                    NoticeLevel::Success
                };
                lines.push(String::new());
                lines.push("--- answer ---".to_owned());
                if matches!(command, Command::GetConfiguration { .. }) {
                    lines.extend(configuration_lines(answer).iter().map(|l| sanitize(l)));
                    lines.push(String::new());
                    lines.push("--- raw answer ---".to_owned());
                }
                if matches!(command, Command::GetInstalledCertificateIds(_)) {
                    lines.extend(certificate_lines(answer).iter().map(|l| sanitize(l)));
                    lines.push(String::new());
                    lines.push("--- raw answer ---".to_owned());
                }
                let body = pretty(answer);
                lines.extend(body.lines().map(sanitize));
                (true, text, level, body, command.answer_is_content(answer))
            }
            Err(e) => {
                lines.push(String::new());
                lines.push("--- error ---".to_owned());
                lines.push(sanitize(&e.0));
                (
                    false,
                    truncate(&sanitize(&e.0), NOTICE_TEXT),
                    NoticeLevel::Error,
                    e.0.clone(),
                    false,
                )
            }
        };
        lines.truncate(RESULT_LINES);
        self.notify(level, format!("{label} on {who}: {summary}"));
        if self.summary(station).is_none() {
            return;
        }
        let record = ResultRecord {
            at: self.now,
            label,
            ok,
            summary,
            lines,
            copy,
        };
        let open = content
            && matches!(self.mode, Mode::Normal)
            && self.selected.as_deref() == Some(station);
        if open {
            self.mode = Mode::Detail(record.detail());
        }
        let list = self.results.entry(station.to_owned()).or_default();
        list.push_front(record);
        list.truncate(RESULTS_PER_STATION);
    }

    /// Drops what is remembered about a forgotten station.
    pub(crate) fn forget_results(&mut self, station: &str) {
        self.results.remove(station);
    }
}

impl ResultRecord {
    pub fn detail(&self) -> Detail {
        Detail {
            title: format!("{}: {}", self.label, self.summary),
            lines: self.lines.clone(),
            scroll: 0,
            copy: self.copy.clone(),
        }
    }
}

/// Palette keys (the list is short, so no paging).
pub fn palette_key(app: &mut App, mut p: Palette, code: KeyCode, ctrl: bool, fx: &mut Vec<Effect>) {
    match code {
        KeyCode::Esc => return,
        KeyCode::Enter => {
            if let Some(id) = p.chosen() {
                app.run_action(id, fx);
            }
            return;
        }
        KeyCode::Up => p.move_by(-1),
        KeyCode::Down => p.move_by(1),
        KeyCode::Char('p') if ctrl => p.move_by(-1),
        KeyCode::Char('n') if ctrl => p.move_by(1),
        KeyCode::Char('u') if ctrl => {
            p.query = TextInput::default();
            p.refresh();
        }
        KeyCode::Char(_) if ctrl => {}
        KeyCode::Char(c) => {
            p.query.insert_char(c);
            p.refresh();
        }
        KeyCode::Backspace => {
            p.query.backspace();
            p.refresh();
        }
        KeyCode::Left => p.query.left(),
        KeyCode::Right => p.query.right(),
        _ => {}
    }
    app.mode = Mode::Palette(p);
}

/// Rows of the results list: newest first.
pub fn results_of<'a>(app: &'a App, station: &str) -> impl Iterator<Item = &'a ResultRecord> {
    app.results.get(station).into_iter().flatten()
}

/// The request payload with secrets replaced (see `kirby_ocpp_core::ocpp::redact_secrets`).
fn redacted_payload(command: &Command) -> Value {
    let frame = Frame::call("", command.action(), command.payload());
    match redact_secrets(&frame) {
        Frame::Call { payload, .. } => payload,
        _ => Value::Null,
    }
}
