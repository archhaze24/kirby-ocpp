//! Modal forms with typed, validated fields, and pick-lists.

use std::sync::{Arc, OnceLock};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::ids::{ConnectorId, IdTag};
use kirby_ocpp_core::ocpp::SchemaRegistry;
use kirby_ocpp_core::ocpp::messages::registry::ACTIONS;
use kirby_ocpp_core::ocpp::messages::{ChargingProfile, IdTagInfo};
use kirby_ocpp_core::ocpp::types::{
    AuthorizationStatus, ChargePointErrorCode, ConfigurationStatus, ConnectorStatus,
    DiagnosticsStatus, FirmwareStatus, Measurand, StopReason,
};
use kirby_ocpp_core::station::config_registry::{ConfigRegistry, ConfigSnapshot};
use kirby_ocpp_core::station::response_rules::canonical_action;
use kirby_ocpp_core::station::security::SECURITY_EVENT_TYPES;
use kirby_ocpp_core::station::{
    ConfigItem, DiagnosticsOutcome, FirmwareOutcome, LocalListEntry, ProfileSummary, ResponseRule,
    Snapshot,
};
use serde_json::Value;

use crate::text::TextInput;

/// Most bytes one form field accepts (typed or pasted); the rest is dropped.
pub const INPUT_CAP: usize = 256 * 1024;

/// Most bytes of the `DataTransfer` `data` field.
pub const DATA_MAX: usize = 64 * 1024;

/// Shown on a field whose input hit [`INPUT_CAP`].
pub const CAP_NOTICE: &str = "input limited to 256 KiB, the rest was dropped";

/// Inserts a pasted string, keeping the field within [`INPUT_CAP`] bytes. Returns `true` when
/// something was dropped.
pub fn insert_str_capped(input: &mut TextInput, s: &str) -> bool {
    let room = INPUT_CAP.saturating_sub(input.text().len());
    let mut end = s.len().min(room);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    input.insert_str(&s[..end]);
    end < s.len()
}

/// Inserts one typed character within [`INPUT_CAP`]. Returns `true` when it was dropped.
pub fn insert_char_capped(input: &mut TextInput, c: char) -> bool {
    if input.text().len() + c.len_utf8() > INPUT_CAP {
        return true;
    }
    input.insert_char(c);
    false
}

/// Length limit of a `DataTransfer` field (`vendorId` 255 and `messageId` 50 characters, as
/// in the schema; `data` is bounded in bytes). `None` when the value fits.
pub fn data_transfer_error(index: usize, value: &str) -> Option<String> {
    let (name, max, len) = match index {
        0 => ("vendorId", 255, value.chars().count()),
        1 => ("messageId", 50, value.chars().count()),
        _ => ("data", DATA_MAX, value.len()),
    };
    (len > max).then(|| {
        let unit = if index > 1 { "bytes" } else { "characters" };
        format!("{name} is {len} {unit}, at most {max}")
    })
}

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
    /// A response rule (`Action=behavior[*N]`) or `Action=off`.
    Rule,
    /// A connector number, 0 (the whole station) included.
    StationConnector,
    /// The value of a configuration key; checked by the core registry.
    ConfigValue,
    /// Optional RFC 3339 timestamp.
    Expiry,
    /// Optional idTag (at most 20 characters).
    OptionalIdTag,
    /// A `csChargingProfiles` JSON object, checked against the `SetChargingProfile` schema.
    ProfileJson,
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
    /// Input was dropped at [`INPUT_CAP`]; shown as the field error until text is removed.
    pub capped: bool,
    /// The value is a secret: it is drawn as `*` (copy, paste and editing work as usual).
    pub secret: bool,
}

/// Configuration keys whose value is a secret and must not be drawn in clear text.
pub fn is_secret_config_key(key: &str) -> bool {
    key.trim().eq_ignore_ascii_case("AuthorizationKey")
}

/// Shown under a hidden field.
pub const SECRET_HINT: &str = "hidden while typing; paste works";

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
            capped: false,
            secret: false,
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
    Rule,
    Raw,
    Search,
    Export,
    LoadScenario,
    ConfigValue,
    LocalEntry,
    Profile,
    SecurityEvent,
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
    Rule(ResponseRule),
    /// `Action=off`: remove the rule of this (canonical) action.
    RuleOff(String),
    SecurityEvent {
        event_type: String,
        tech_info: Option<String>,
    },
    Search(String),
    Export(String),
    LoadScenario(String),
    ConfigValue {
        key: String,
        value: String,
    },
    LocalEntry {
        id_tag: IdTag,
        info: IdTagInfo,
    },
    Profile {
        connector: ConnectorId,
        profile: Box<ChargingProfile>,
    },
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
    /// What the form edits (the configuration key).
    pub context: String,
    /// The station's configuration as far as the snapshot shows it, to check values with.
    checker: Option<Box<ConfigRegistry>>,
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

    pub fn security_event() -> Self {
        let options = SECURITY_EVENT_TYPES
            .iter()
            .map(|t| (*t).to_owned())
            .collect();
        Self::build(
            FormKind::SecurityEvent,
            "Security event",
            vec![
                Field::new("event type", FieldKind::Select, "").with_options(options),
                Field::new(
                    "techInfo (optional, at most 255 characters)",
                    FieldKind::OptionalText,
                    "",
                ),
            ],
            1,
        )
    }

    pub fn rule() -> Self {
        Self::build(
            FormKind::Rule,
            "Response rule",
            vec![Field::new(
                "Action=reject | delay:MS | error:Code[:text] | drop | ignore [*N]; Action=off clears",
                FieldKind::Rule,
                "",
            )],
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

    /// Edit the value of a configuration key. Values are checked with the core registry,
    /// loaded with the writable values of the snapshot.
    pub fn config_value(item: &ConfigItem, snapshot: &Snapshot) -> Self {
        let station = StationConfig::new("ws://localhost/ocpp")
            .with_connectors(u32::try_from(snapshot.connectors.len()).unwrap_or(1).max(1));
        let mut registry = ConfigRegistry::new(&station);
        registry.restore(&ConfigSnapshot {
            values: snapshot
                .config
                .iter()
                .filter(|i| !i.readonly)
                .map(|i| (i.key.clone(), i.value.clone()))
                .collect(),
        });
        let mut form = Self::build(
            FormKind::ConfigValue,
            &format!("Set {} ({})", item.key, item.hint),
            vec![Field::new("value", FieldKind::ConfigValue, &item.value)],
            1,
        );
        form.context.clone_from(&item.key);
        if let Some(field) = form.fields.first_mut() {
            field.secret = is_secret_config_key(&item.key);
        }
        form.checker = Some(Box::new(registry));
        form.validate_all();
        form
    }

    /// Add or replace a local list entry; `current` prefills an edit.
    pub fn local_entry(current: Option<&LocalListEntry>) -> Self {
        let options: Vec<String> = AuthorizationStatus::ALL
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        let mut status = Field::new("status", FieldKind::Select, "").with_options(options);
        if let Some(e) = current {
            status.option = AuthorizationStatus::ALL
                .iter()
                .position(|s| *s == e.status)
                .unwrap_or(0);
        }
        let expiry = current
            .and_then(|e| e.expiry_date)
            .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default();
        let parent = current
            .and_then(|e| e.parent_id_tag.clone())
            .unwrap_or_default();
        let mut form = Self::build(
            FormKind::LocalEntry,
            if current.is_some() {
                "Edit local list entry (a changed idTag adds a new entry)"
            } else {
                "Add local list entry"
            },
            vec![
                Field::new(
                    "idTag",
                    FieldKind::IdTag,
                    current.map_or("", |e| e.id_tag.as_str()),
                ),
                status,
                Field::new("expiry (RFC 3339, optional)", FieldKind::Expiry, &expiry),
                Field::new("parentIdTag (optional)", FieldKind::OptionalIdTag, &parent),
            ],
            1,
        );
        form.validate_all();
        form
    }

    /// Install a charging profile from JSON; `current` prefills an edit, else a template.
    pub fn profile(current: Option<&ProfileSummary>, max_connector: u32) -> Self {
        let json = current.map_or(PROFILE_TEMPLATE, |p| p.json.as_str());
        let connector = current.map_or(0, |p| p.connector.get());
        let mut form = Self::build(
            FormKind::Profile,
            "Install charging profile (SetChargingProfile rules)",
            vec![
                Field::new(
                    "connector (0 = station)",
                    FieldKind::StationConnector,
                    &connector.to_string(),
                ),
                Field::new("csChargingProfiles (JSON)", FieldKind::ProfileJson, json),
            ],
            max_connector,
        );
        form.validate_all();
        form
    }

    fn build(kind: FormKind, title: &str, fields: Vec<Field>, max_connector: u32) -> Self {
        Self {
            kind,
            title: title.to_owned(),
            fields,
            focus: 0,
            max_connector,
            context: String::new(),
            checker: None,
        }
    }

    pub fn paste(&mut self, text: &str) {
        let f = &mut self.fields[self.focus];
        if f.is_text() {
            f.capped |= insert_str_capped(&mut f.input, text);
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
                        f.capped |= insert_char_capped(&mut f.input, c);
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
        if matches!(self.kind, FormKind::Raw | FormKind::Profile) {
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
        let form_kind = self.kind;
        let action = (self.kind == FormKind::Raw)
            .then(|| self.fields[0].options.get(self.fields[0].option).cloned())
            .flatten();
        let config_error = (self.kind == FormKind::ConfigValue)
            .then(|| {
                let value = self.fields[i].input.text();
                self.checker
                    .as_ref()
                    .filter(|c| c.check(&self.context, value) != ConfigurationStatus::Accepted)
                    .map(|c| {
                        let hint = c.get(&self.context).map_or("", |e| e.hint());
                        format!("not accepted: expects {hint}")
                    })
            })
            .flatten();
        let connector_text =
            (self.kind == FormKind::Profile).then(|| self.fields[0].input.text().trim().to_owned());
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
            FieldKind::Rule => parse_rule_input(&text).err(),
            FieldKind::StationConnector => match text.parse::<u32>() {
                Ok(n) if n <= max => None,
                _ => Some(format!("connector must be a number from 0 to {max}")),
            },
            FieldKind::ConfigValue => config_error,
            FieldKind::Expiry => (!text.is_empty())
                .then(|| chrono::DateTime::parse_from_rfc3339(&text).err())
                .flatten()
                .map(|_| "use RFC 3339, e.g. 2030-01-01T00:00:00Z".to_owned()),
            FieldKind::OptionalIdTag => (!text.is_empty())
                .then(|| IdTag::new(text).err().map(|e| e.to_string()))
                .flatten(),
            FieldKind::ProfileJson => {
                validate_profile(f.input.text(), connector_text.as_deref().unwrap_or("0"))
            }
            FieldKind::OptionalText | FieldKind::Select | FieldKind::Multi => None,
        };
        if f.error.is_none() && form_kind == FormKind::DataTransfer {
            let value = if i == 2 {
                f.input.text()
            } else {
                f.input.text().trim()
            };
            f.error = data_transfer_error(i, value);
        }
        if f.input.text().len() < INPUT_CAP {
            f.capped = false;
        }
        if f.capped {
            f.error = Some(match f.error.take() {
                Some(e) => format!("{CAP_NOTICE}; {e}"),
                None => CAP_NOTICE.to_owned(),
            });
        }
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
                // Free text: spaces at the ends are part of the value.
                data: Some(self.fields[2].input.text().to_owned()).filter(|s| !s.trim().is_empty()),
            },
            FormKind::Rule => match parse_rule_input(&text(0)).ok()? {
                RuleInput::Set(rule) => Submitted::Rule(rule),
                RuleInput::Off(action) => Submitted::RuleOff(action),
            },
            FormKind::Raw => Submitted::Raw {
                action: self.fields[0].options.get(self.fields[0].option)?.clone(),
                payload: serde_json::from_str(self.fields[1].input.text()).ok()?,
            },
            FormKind::Search => Submitted::Search(self.fields[0].input.text().trim().to_owned()),
            FormKind::Export => Submitted::Export(text(0)),
            FormKind::LoadScenario => Submitted::LoadScenario(text(0)),
            FormKind::ConfigValue => Submitted::ConfigValue {
                key: self.context.clone(),
                // Free text: spaces at the ends are part of the value.
                value: self.fields[0].input.text().to_owned(),
            },
            FormKind::LocalEntry => Submitted::LocalEntry {
                id_tag: IdTag::new(text(0)).ok()?,
                info: IdTagInfo {
                    status: *AuthorizationStatus::ALL.get(self.fields[1].option)?,
                    expiry_date: opt(2)
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
                        .map(|d| d.with_timezone(&chrono::Utc)),
                    parent_id_tag: opt(3).and_then(|t| IdTag::new(t).ok()),
                },
            },
            FormKind::SecurityEvent => Submitted::SecurityEvent {
                event_type: self.fields[0].options.get(self.fields[0].option)?.clone(),
                tech_info: opt(1),
            },
            FormKind::Profile => Submitted::Profile {
                connector: ConnectorId::new(text(0).parse().ok()?),
                profile: Box::new(serde_json::from_str(self.fields[1].input.text()).ok()?),
            },
        })
    }
}

enum RuleInput {
    Set(ResponseRule),
    Off(String),
}

/// `Action=off` removes a rule; anything else is the compact rule syntax of the core.
fn parse_rule_input(text: &str) -> Result<RuleInput, String> {
    if let Some((action, rest)) = text.split_once('=')
        && rest.trim().eq_ignore_ascii_case("off")
    {
        return canonical_action(action)
            .map(|a| RuleInput::Off(a.to_owned()))
            .map_err(|e| e.to_string());
    }
    text.parse::<ResponseRule>()
        .map(RuleInput::Set)
        .map_err(|e| e.to_string())
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

/// An example for the install form: a 16 A station-wide cap.
pub const PROFILE_TEMPLATE: &str = r#"{"chargingProfileId":1,"stackLevel":0,"chargingProfilePurpose":"ChargePointMaxProfile","chargingProfileKind":"Absolute","chargingSchedule":{"chargingRateUnit":"A","chargingSchedulePeriod":[{"startPeriod":0,"limit":16.0}]}}"#;

/// `None` when `text` is a `csChargingProfiles` object that satisfies the `SetChargingProfile`
/// request schema (with `connector`) and fits the typed profile.
fn validate_profile(text: &str, connector: &str) -> Option<String> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Some(format!("invalid JSON: {e}")),
    };
    if !value.is_object() {
        return Some("the profile must be a JSON object".to_owned());
    }
    let connector_id: u32 = connector.parse().unwrap_or(0);
    let request = serde_json::json!({"connectorId": connector_id, "csChargingProfiles": value});
    if let Err(e) = schemas().validate_request("SetChargingProfile", &request) {
        return Some(e.to_string());
    }
    serde_json::from_value::<ChargingProfile>(value)
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

    #[test]
    fn data_transfer_limits_are_checked_inline() {
        let mut f = Form::data_transfer(&"v".repeat(256));
        assert!(f.fields[0].error.is_none(), "set only by validation");
        assert!(f.submit().is_none());
        assert!(f.fields[0].error.as_ref().unwrap().contains("at most 255"));
        let mut f = Form::data_transfer("Acme");
        f.focus = 1;
        f.paste(&"m".repeat(51));
        assert!(f.fields[1].error.as_ref().unwrap().contains("at most 50"));
        f.focus = 2;
        f.paste(&"d".repeat(DATA_MAX + 1));
        assert!(
            f.fields[2]
                .error
                .as_ref()
                .unwrap()
                .contains("at most 65536")
        );
        assert!(f.submit().is_none());
        let mut f = Form::data_transfer("Acme");
        f.focus = 2;
        f.paste(&"d".repeat(DATA_MAX));
        assert!(f.submit().is_some());
    }

    #[test]
    fn free_text_values_keep_their_spaces() {
        let mut f = Form::data_transfer("Acme");
        f.focus = 2;
        f.paste("  a b  ");
        let Some(Submitted::Data { data, .. }) = f.submit() else {
            panic!("valid");
        };
        assert_eq!(data.as_deref(), Some("  a b  "));
        let mut f = Form::data_transfer("  Acme ");
        f.focus = 2;
        f.paste("   ");
        let Some(Submitted::Data {
            vendor_id, data, ..
        }) = f.submit()
        else {
            panic!("valid");
        };
        assert_eq!(vendor_id, "Acme", "ids are still trimmed");
        assert_eq!(data, None, "blank means absent");
    }

    #[test]
    fn paste_is_capped_with_a_visible_notice() {
        let mut f = Form::raw(None);
        f.focus = 1;
        f.paste(&"x".repeat(INPUT_CAP + 5000));
        assert_eq!(f.fields[1].input.text().len(), INPUT_CAP);
        assert!(f.fields[1].capped);
        f.handle_key(key(KeyCode::Char('y')));
        assert_eq!(f.fields[1].input.text().len(), INPUT_CAP);
        f.handle_key(key(KeyCode::Backspace));
        assert!(!f.fields[1].capped, "room again");
    }

    #[test]
    fn capped_notice_shows_when_nothing_else_is_wrong() {
        let mut f = Form::search("");
        f.paste(&"x".repeat(INPUT_CAP + 1));
        assert_eq!(f.fields[0].error.as_deref(), Some(CAP_NOTICE));
    }

    #[test]
    fn capped_notice_comes_first_next_to_another_error() {
        let mut f = Form::raw(None);
        f.focus = 1;
        f.paste(&"x".repeat(INPUT_CAP + 5));
        let err = f.fields[1].error.clone().unwrap_or_default();
        assert!(err.starts_with(CAP_NOTICE), "{err}");
        assert!(
            err.len() > CAP_NOTICE.len(),
            "the other error follows: {err}"
        );
    }

    #[test]
    fn authorization_key_editor_hides_the_value() {
        let config = StationConfig::new("ws://localhost/ocpp");
        let snapshot = Snapshot::initial(&config);
        let item = |key: &str| ConfigItem {
            key: key.to_owned(),
            value: String::new(),
            readonly: false,
            hint: "text".to_owned(),
        };
        assert!(Form::config_value(&item("AuthorizationKey"), &snapshot).fields[0].secret);
        assert!(!Form::config_value(&item("HeartbeatInterval"), &snapshot).fields[0].secret);
        assert!(is_secret_config_key(" authorizationkey "));
    }
}
