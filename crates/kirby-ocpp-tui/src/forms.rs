//! Modal forms with typed, validated fields, and pick-lists.

use std::sync::{Arc, OnceLock};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirby_ocpp_core::ids::{ConnectorId, IdTag};
use kirby_ocpp_core::ocpp::SchemaRegistry;
use kirby_ocpp_core::ocpp::messages::registry::ACTIONS;
use kirby_ocpp_core::ocpp::types::{
    ChargePointErrorCode, ConnectorStatus, DiagnosticsStatus, FirmwareStatus, Measurand, StopReason,
};
use kirby_ocpp_core::station::{DiagnosticsOutcome, FirmwareOutcome};
use serde_json::Value;

use crate::text::TextInput;

/// The schema registry is expensive to build, so it is shared.
pub fn schemas() -> Arc<SchemaRegistry> {
    static REG: OnceLock<Arc<SchemaRegistry>> = OnceLock::new();
    REG.get_or_init(|| Arc::new(SchemaRegistry::new().expect("embedded schemas are valid")))
        .clone()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    OptionalText,
    Connector,
    IdTag,
    Wh,
    /// A JSON object payload.
    Json,
    /// One of `options` (Left/Right, or type the first letter).
    Select,
    /// Any number of `options` (Left/Right moves, Space toggles).
    Multi,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub label: &'static str,
    pub kind: FieldKind,
    pub input: TextInput,
    pub options: Vec<String>,
    /// Current option (`Select`) or option cursor (`Multi`).
    pub option: usize,
    pub checked: Vec<bool>,
    pub error: Option<String>,
}

impl Field {
    fn new(label: &'static str, kind: FieldKind, value: &str) -> Self {
        Self {
            label,
            kind,
            input: TextInput::new(value),
            options: Vec::new(),
            option: 0,
            checked: Vec::new(),
            error: None,
        }
    }

    fn with_options(mut self, options: Vec<String>) -> Self {
        self.checked = vec![false; options.len()];
        self.options = options;
        self
    }

    pub fn is_text(&self) -> bool {
        !matches!(self.kind, FieldKind::Select | FieldKind::Multi)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKind {
    Authorize,
    StartTx,
    MeterCustom,
    DataTransfer,
    Raw,
    Search,
    Export,
    LoadScenario,
}

/// What a valid form produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Submitted {
    Authorize {
        id_tag: IdTag,
    },
    StartTx {
        connector: ConnectorId,
        id_tag: IdTag,
    },
    Meter {
        connector: ConnectorId,
        wh_delta: u64,
        measurands: Option<Vec<Measurand>>,
    },
    Data {
        vendor_id: String,
        message_id: Option<String>,
        data: Option<String>,
    },
    Raw {
        action: String,
        payload: Value,
    },
    Search(String),
    Export(String),
    LoadScenario(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormEvent {
    None,
    Cancel,
    Submit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub kind: FormKind,
    pub title: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    /// Highest valid connector number.
    pub max_connector: u32,
}

impl Form {
    pub fn authorize() -> Self {
        Self::build(
            FormKind::Authorize,
            "Authorize",
            vec![Field::new("idTag", FieldKind::IdTag, "")],
            1,
        )
    }

    pub fn start_tx(connector: ConnectorId, max_connector: u32) -> Self {
        Self::build(
            FormKind::StartTx,
            "Start transaction",
            vec![
                Field::new(
                    "connector",
                    FieldKind::Connector,
                    &connector.get().to_string(),
                ),
                Field::new("idTag", FieldKind::IdTag, ""),
            ],
            max_connector,
        )
    }

    pub fn meter_custom(connector: ConnectorId, max_connector: u32) -> Self {
        Self::build(
            FormKind::MeterCustom,
            "MeterValues",
            vec![
                Field::new(
                    "connector",
                    FieldKind::Connector,
                    &connector.get().to_string(),
                ),
                Field::new("Wh delta", FieldKind::Wh, "120"),
                Field::new("measurands", FieldKind::Multi, "").with_options(
                    Measurand::ALL
                        .iter()
                        .map(|m| m.as_str().to_owned())
                        .collect(),
                ),
            ],
            max_connector,
        )
    }

    pub fn data_transfer(vendor: &str) -> Self {
        Self::build(
            FormKind::DataTransfer,
            "DataTransfer",
            vec![
                Field::new("vendorId", FieldKind::Text, vendor),
                Field::new("messageId (optional)", FieldKind::OptionalText, ""),
                Field::new("data (optional)", FieldKind::OptionalText, ""),
            ],
            1,
        )
    }

    pub fn raw(initial_action: Option<&str>) -> Self {
        let options: Vec<String> = ACTIONS.iter().map(|a| a.name.to_owned()).collect();
        let mut action = Field::new("action", FieldKind::Select, "").with_options(options);
        if let Some(i) = initial_action.and_then(|a| action.options.iter().position(|o| o == a)) {
            action.option = i;
        }
        let mut form = Self::build(
            FormKind::Raw,
            "Raw CALL",
            vec![action, Field::new("payload (JSON)", FieldKind::Json, "{}")],
            1,
        );
        form.validate_all();
        form
    }

    pub fn search(current: &str) -> Self {
        Self::build(
            FormKind::Search,
            "Search log",
            vec![Field::new("text", FieldKind::OptionalText, current)],
            1,
        )
    }

    pub fn export(default_path: &str) -> Self {
        Self::build(
            FormKind::Export,
            "Export log (JSONL)",
            vec![Field::new("path", FieldKind::Path, default_path)],
            1,
        )
    }

    pub fn load_scenario() -> Self {
        Self::build(
            FormKind::LoadScenario,
            "Run scenario file",
            vec![Field::new("path", FieldKind::Path, "")],
            1,
        )
    }

    fn build(kind: FormKind, title: &str, fields: Vec<Field>, max_connector: u32) -> Self {
        Self {
            kind,
            title: title.to_owned(),
            fields,
            focus: 0,
            max_connector,
        }
    }

    pub fn paste(&mut self, text: &str) {
        let f = &mut self.fields[self.focus];
        if f.is_text() {
            f.input.insert_str(text);
            self.validate_field(self.focus);
            self.after_edit();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormEvent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return FormEvent::Cancel,
            KeyCode::Enter => return FormEvent::Submit,
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % self.fields.len(),
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = (self.focus + self.fields.len() - 1) % self.fields.len();
            }
            code => {
                let f = &mut self.fields[self.focus];
                match (f.kind, code) {
                    (FieldKind::Select | FieldKind::Multi, KeyCode::Left) => {
                        step(&mut f.option, f.options.len(), -1);
                    }
                    (FieldKind::Select | FieldKind::Multi, KeyCode::Right) => {
                        step(&mut f.option, f.options.len(), 1);
                    }
                    (FieldKind::Select, KeyCode::Char(c)) if !ctrl => {
                        let lc = c.to_ascii_lowercase();
                        let n = f.options.len();
                        if let Some(i) = (1..=n).map(|d| (f.option + d) % n).find(|&i| {
                            f.options[i]
                                .chars()
                                .next()
                                .is_some_and(|o| o.to_ascii_lowercase() == lc)
                        }) {
                            f.option = i;
                        }
                    }
                    (FieldKind::Multi, KeyCode::Char(' ')) => {
                        let i = f.option;
                        f.checked[i] = !f.checked[i];
                    }
                    (FieldKind::Select | FieldKind::Multi, _) => {}
                    (_, KeyCode::Left) => f.input.left(),
                    (_, KeyCode::Right) => f.input.right(),
                    (_, KeyCode::Home) => f.input.home(),
                    (_, KeyCode::End) => f.input.end(),
                    (_, KeyCode::Backspace) => f.input.backspace(),
                    (_, KeyCode::Delete) => f.input.delete(),
                    (_, KeyCode::Char('a')) if ctrl => f.input.home(),
                    (_, KeyCode::Char('e')) if ctrl => f.input.end(),
                    (_, KeyCode::Char('u')) if ctrl => {
                        *f = Field {
                            input: TextInput::default(),
                            ..f.clone()
                        }
                    }
                    (_, KeyCode::Char(c))
                        if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        f.input.insert_char(c);
                    }
                    _ => {}
                }
                self.validate_field(self.focus);
                self.after_edit();
            }
        }
        FormEvent::None
    }

    /// Fields that depend on each other (raw action + payload) re-validate together.
    fn after_edit(&mut self) {
        if self.kind == FormKind::Raw {
            self.validate_field(1);
        }
    }

    pub fn validate_all(&mut self) -> bool {
        for i in 0..self.fields.len() {
            self.validate_field(i);
        }
        self.fields.iter().all(|f| f.error.is_none())
    }

    fn validate_field(&mut self, i: usize) {
        let max = self.max_connector;
        let action = (self.kind == FormKind::Raw)
            .then(|| self.fields[0].options.get(self.fields[0].option).cloned())
            .flatten();
        let f = &mut self.fields[i];
        let text = f.input.text().trim().to_owned();
        f.error = match f.kind {
            FieldKind::Connector => match text.parse::<u32>() {
                Ok(n) if (1..=max).contains(&n) => None,
                _ => Some(format!("connector must be a number from 1 to {max}")),
            },
            FieldKind::IdTag => {
                if text.is_empty() {
                    Some("idTag is required".to_owned())
                } else {
                    IdTag::new(text).err().map(|e| e.to_string())
                }
            }
            FieldKind::Wh => text
                .parse::<u64>()
                .err()
                .map(|_| "Wh delta must be a whole number, 0 or more".to_owned()),
            FieldKind::Text | FieldKind::Path => text.is_empty().then(|| "required".to_owned()),
            FieldKind::Json => validate_payload(f.input.text(), action.as_deref()),
            FieldKind::OptionalText | FieldKind::Select | FieldKind::Multi => None,
        };
    }

    /// Validates and converts the form. On failure the errors are set on the fields.
    pub fn submit(&mut self) -> Option<Submitted> {
        if !self.validate_all() {
            if let Some(i) = self.fields.iter().position(|f| f.error.is_some()) {
                self.focus = i;
            }
            return None;
        }
        let text = |i: usize| self.fields[i].input.text().trim().to_owned();
        let opt = |i: usize| Some(text(i)).filter(|s| !s.is_empty());
        let connector = |i: usize| ConnectorId::new(text(i).parse().unwrap_or(1));
        Some(match self.kind {
            FormKind::Authorize => Submitted::Authorize {
                id_tag: IdTag::new(text(0)).ok()?,
            },
            FormKind::StartTx => Submitted::StartTx {
                connector: connector(0),
                id_tag: IdTag::new(text(1)).ok()?,
            },
            FormKind::MeterCustom => {
                let f = &self.fields[2];
                let picked: Vec<Measurand> = Measurand::ALL
                    .iter()
                    .zip(&f.checked)
                    .filter(|(_, on)| **on)
                    .map(|(m, _)| *m)
                    .collect();
                Submitted::Meter {
                    connector: connector(0),
                    wh_delta: text(1).parse().ok()?,
                    measurands: (!picked.is_empty()).then_some(picked),
                }
            }
            FormKind::DataTransfer => Submitted::Data {
                vendor_id: text(0),
                message_id: opt(1),
                data: opt(2),
            },
            FormKind::Raw => Submitted::Raw {
                action: self.fields[0].options.get(self.fields[0].option)?.clone(),
                payload: serde_json::from_str(self.fields[1].input.text()).ok()?,
            },
            FormKind::Search => Submitted::Search(self.fields[0].input.text().trim().to_owned()),
            FormKind::Export => Submitted::Export(text(0)),
            FormKind::LoadScenario => Submitted::LoadScenario(text(0)),
        })
    }
}

fn step(i: &mut usize, n: usize, d: isize) {
    if n > 0 {
        *i = (*i + n).wrapping_add_signed(d) % n;
    }
}

/// `None` when `text` is a JSON object that satisfies the request schema of `action`.
fn validate_payload(text: &str, action: Option<&str>) -> Option<String> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Some(format!("invalid JSON: {e}")),
    };
    if !value.is_object() {
        return Some("payload must be a JSON object".to_owned());
    }
    let action = action?;
    schemas()
        .validate_request(action, &value)
        .err()
        .map(|e| e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKind {
    SetStatus,
    Fault,
    StopReason,
    DiagOutcome,
    FwOutcome,
    DiagStatus,
    FwStatus,
}

/// A pick-list opened for one connector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub kind: ChoiceKind,
    pub connector: ConnectorId,
    pub title: String,
    pub items: Vec<String>,
    pub selected: usize,
}

impl Choice {
    pub fn new(kind: ChoiceKind, connector: ConnectorId) -> Self {
        let (title, items): (&str, Vec<String>) = match kind {
            ChoiceKind::SetStatus => (
                "Set connector status",
                names(ConnectorStatus::ALL.iter().map(|s| s.as_str())),
            ),
            ChoiceKind::Fault => (
                "Inject fault",
                names(
                    ChargePointErrorCode::ALL
                        .iter()
                        .filter(|c| **c != ChargePointErrorCode::NoError)
                        .map(|s| s.as_str()),
                ),
            ),
            ChoiceKind::StopReason => (
                "Stop reason",
                names(StopReason::ALL.iter().map(|s| s.as_str())),
            ),
            ChoiceKind::DiagOutcome => (
                "Next diagnostics outcome",
                DiagnosticsOutcome::ALL
                    .iter()
                    .map(|o| format!("{o:?}"))
                    .collect(),
            ),
            ChoiceKind::FwOutcome => (
                "Next firmware outcome",
                FirmwareOutcome::ALL
                    .iter()
                    .map(|o| format!("{o:?}"))
                    .collect(),
            ),
            ChoiceKind::DiagStatus => (
                "DiagnosticsStatus",
                names(DiagnosticsStatus::ALL.iter().map(|s| s.as_str())),
            ),
            ChoiceKind::FwStatus => (
                "FirmwareStatus",
                names(FirmwareStatus::ALL.iter().map(|s| s.as_str())),
            ),
        };
        Self {
            kind,
            connector,
            title: title.to_owned(),
            items,
            selected: 0,
        }
    }

    pub fn move_by(&mut self, d: isize) {
        step(&mut self.selected, self.items.len(), d);
    }
}

fn names<'a>(it: impl Iterator<Item = &'a str>) -> Vec<String> {
    it.map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEventKind;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    fn type_text(form: &mut Form, s: &str) {
        for c in s.chars() {
            form.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn id_tag_is_required_and_limited_to_20_chars() {
        let mut f = Form::authorize();
        assert!(f.submit().is_none());
        assert_eq!(f.fields[0].error.as_deref(), Some("idTag is required"));
        type_text(&mut f, &"x".repeat(21));
        assert!(f.fields[0].error.as_ref().unwrap().contains("21"));
        f.handle_key(key(KeyCode::Backspace));
        assert!(f.fields[0].error.is_none());
        assert!(matches!(f.submit(), Some(Submitted::Authorize { .. })));
    }

    #[test]
    fn unicode_id_tag_counts_characters_and_paste_is_validated() {
        let mut f = Form::authorize();
        f.paste(&"é".repeat(20));
        assert!(f.fields[0].error.is_none());
        f.paste("日\nx");
        assert!(f.fields[0].error.is_some());
        assert!(!f.fields[0].input.text().contains('\n'));
    }

    #[test]
    fn connector_range_and_wh_are_checked() {
        let mut f = Form::meter_custom(ConnectorId::new(1), 2);
        f.focus = 0;
        f.handle_key(key(KeyCode::Backspace));
        type_text(&mut f, "3");
        assert!(f.fields[0].error.as_ref().unwrap().contains("1 to 2"));
        f.handle_key(key(KeyCode::Backspace));
        type_text(&mut f, "2");
        f.handle_key(key(KeyCode::Tab));
        f.handle_key(key(KeyCode::Backspace));
        type_text(&mut f, "-5");
        assert!(f.fields[1].error.is_some());
        assert!(f.submit().is_none());
        assert_eq!(f.focus, 1);
    }

    #[test]
    fn measurands_multi_select_collects_checked_options() {
        let mut f = Form::meter_custom(ConnectorId::new(1), 1);
        f.focus = 2;
        f.handle_key(key(KeyCode::Char(' ')));
        f.handle_key(key(KeyCode::Right));
        f.handle_key(key(KeyCode::Char(' ')));
        let Some(Submitted::Meter {
            measurands: Some(m),
            wh_delta: 120,
            ..
        }) = f.submit()
        else {
            panic!("expected meter values");
        };
        assert_eq!(m, [Measurand::ALL[0], Measurand::ALL[1]]);
    }

    #[test]
    fn raw_payload_is_validated_live_against_the_schema() {
        let mut f = Form::raw(Some("Authorize"));
        assert!(f.fields[1].error.is_some(), "{{}} lacks idTag");
        f.focus = 1;
        f.handle_key(key(KeyCode::End));
        f.handle_key(key(KeyCode::Backspace));
        type_text(&mut f, "\"idTag\":\"A\"}");
        assert_eq!(f.fields[1].error, None);
        f.paste("}");
        assert!(f.fields[1].error.as_ref().unwrap().contains("invalid JSON"));
        // switching the action re-validates the payload against the other schema
        f.fields[1].input = TextInput::new("{\"idTag\":\"A\"}");
        f.focus = 0;
        f.handle_key(key(KeyCode::Char('h'))); // Heartbeat: additionalProperties=false
        assert_eq!(f.fields[0].options[f.fields[0].option], "Heartbeat");
        assert!(f.fields[1].error.is_some());
    }

    #[test]
    fn data_transfer_needs_a_vendor_and_optional_fields_may_be_empty() {
        let mut f = Form::data_transfer("");
        assert!(f.submit().is_none());
        type_text(&mut f, "Acme");
        let Some(Submitted::Data {
            vendor_id,
            message_id: None,
            data: None,
        }) = f.submit()
        else {
            panic!("expected data transfer");
        };
        assert_eq!(vendor_id, "Acme");
    }

    #[test]
    fn escape_cancels_and_enter_submits() {
        let mut f = Form::search("");
        assert_eq!(f.handle_key(key(KeyCode::Esc)), FormEvent::Cancel);
        assert_eq!(f.handle_key(key(KeyCode::Enter)), FormEvent::Submit);
    }

    #[test]
    fn choices_wrap_and_fault_list_excludes_no_error() {
        let mut c = Choice::new(ChoiceKind::Fault, ConnectorId::new(1));
        assert!(!c.items.iter().any(|i| i == "NoError"));
        c.move_by(-1);
        assert_eq!(c.selected, c.items.len() - 1);
    }
}
