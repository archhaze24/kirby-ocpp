//! Modal forms with typed fields and live validation, one per action on a station or on the
//! server's scripted behaviour. The key handling follows the emulator's forms; the field set
//! is its own (the emulator's `Form` is tied to its station actions).

use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirby_csms_core::registry;
use kirby_csms_core::{ACTIONS_PROTOCOL, MAX_STATION_ID_LEN};
use kirby_ocpp_core::ids::{ConnectorId, IdTag, ReservationId};
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::SchemaRegistry;
use kirby_ocpp_core::ocpp::messages::{
    AuthorizationData, CertificateHashData, CertificateUse, ChargingProfile,
    ClearChargingProfileRequest, ExtendedMessageTrigger, GetDiagnosticsRequest, GetLogRequest,
    HashAlgorithm, IdTagInfo, LogParameters, LogType, MAX_CERTIFICATE_CHAIN_LEN,
    MAX_CERTIFICATE_LEN, ReserveNowRequest, SendLocalListRequest, SignedFirmware,
    SignedUpdateFirmwareRequest, UpdateFirmwareRequest,
};
use kirby_ocpp_core::ocpp::types::{
    AuthorizationStatus, AvailabilityType, ChargingProfilePurposeType, ChargingRateUnitType,
    MessageTrigger, ResetType, UpdateType,
};
use kirby_ocpp_tui::forms::{
    CAP_NOTICE, INPUT_CAP, PROFILE_TEMPLATE, data_transfer_error, insert_char_capped,
    insert_str_capped, is_secret_config_key, schemas,
};
use kirby_ocpp_tui::text::TextInput;
use serde_json::Value;

use crate::actions::ScopedRule;
use crate::command::Command;

/// The label of the optional station field of the rule and fault forms.
const STATION_LABEL: &str = "station (optional, empty = all)";

/// Longest configuration key the schema allows.
const KEY_MAX: usize = 50;
/// Most occurrences one injected fault may cover.
const TIMES_MAX: usize = 1_000_000;
/// Most entries a `SendLocalList` form may carry.
const AUTH_LIST_MAX: usize = 100;
/// The "leave it out" choice of an optional select.
const ANY: &str = "(any)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    OptionalText,
    IdTag,
    /// An `idTag` that may be left empty.
    OptionalIdTag,
    /// A local authorization list: comma separated `idTag[:status]` items (status defaults to
    /// Accepted; `idTag:-` has no `idTagInfo`, which deletes the entry in a differential
    /// update), at most [`AUTH_LIST_MAX`] items; may be left empty.
    AuthList,
    /// A connector number from `min`; may be left empty when `optional`.
    Connector {
        min: u32,
        optional: bool,
    },
    /// A count of at least 1.
    Times,
    /// A JSON object.
    Json,
    /// A JSON object checked against the request schema of the action in field 0.
    RawJson,
    /// A `csChargingProfiles` object checked as part of `SetChargingProfile` (connector in
    /// field 0).
    ProfileJson,
    /// A JSON object, checked only while the fault kind in field 1 is `invalid_payload`.
    FaultJson,
    /// Optional configuration keys separated by commas or spaces.
    KeyList,
    /// One of `options` (Left/Right, or the first letter).
    Select,
    /// One or more PEM blocks, at most `max` characters once normalised. A paste turns line
    /// breaks into spaces; the blocks are rebuilt on submit.
    Pem {
        max: usize,
    },
    /// A station id (at most [`MAX_STATION_ID_LEN`] bytes, no control characters); may be
    /// left empty (every station).
    Station,
    /// A whole number from 0 (a request id, retries); may be left empty when `optional`.
    Count {
        optional: bool,
    },
    /// An RFC 3339 time; may be left empty when `optional`.
    Timestamp {
        optional: bool,
    },
    /// A typed input of a registry form: what it accepts (text, whole number, JSON value,
    /// time, PEM) and whether it may stay empty come from its [`Field::spec`].
    Spec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub label: &'static str,
    pub kind: FieldKind,
    pub input: TextInput,
    pub options: Vec<String>,
    pub option: usize,
    pub error: Option<String>,
    /// Input was dropped at the size cap; shown as the field error until text is removed.
    pub capped: bool,
    /// A short note for the person filling the field (registry forms); empty when none.
    pub hint: &'static str,
    /// The registry field behind this input (registry forms only).
    pub spec: Option<&'static registry::FieldSpec>,
    /// What each option stands for when it differs from its text (the transaction ids behind
    /// the options of `RequestStopTransaction`); empty when the options are the values.
    pub values: Vec<String>,
    /// The field was edited (or the form was submitted): a problem is shown as an error. Until
    /// then an empty required field reads as a hint.
    pub touched: bool,
    /// A selector with a single option stays visible (it names the target, for instance the
    /// one running transaction); without this flag it is hidden and skipped.
    pub keep_single: bool,
}

impl Field {
    /// The payload of a raw call, starting with `template`; empty means `{}`.
    fn raw_payload(template: &str) -> Self {
        Self {
            hint: "empty sends {}",
            ..Self::new("payload (JSON)", FieldKind::RawJson, template)
        }
    }

    /// A selector with a single option that names no target: nothing to choose, so the form
    /// neither draws nor focuses it.
    pub fn is_hidden(&self) -> bool {
        self.kind == FieldKind::Select && self.options.len() == 1 && !self.keep_single
    }

    /// An empty field nobody edited yet: its problem (`required`) is shown as a hint.
    pub fn is_quiet(&self) -> bool {
        !self.touched && self.input.is_empty()
    }

    fn new(label: &'static str, kind: FieldKind, value: &str) -> Self {
        Self {
            label,
            kind,
            input: TextInput::new(value),
            options: Vec::new(),
            option: 0,
            error: None,
            capped: false,
            hint: "",
            spec: None,
            values: Vec::new(),
            touched: false,
            keep_single: false,
        }
    }

    fn select(label: &'static str, options: Vec<String>) -> Self {
        Self {
            options,
            ..Self::new(label, FieldKind::Select, "")
        }
    }

    pub fn is_text(&self) -> bool {
        self.kind != FieldKind::Select
    }

    pub fn chosen(&self) -> Option<&str> {
        self.options.get(self.option).map(String::as_str)
    }

    /// The value behind the chosen option: its `values` entry, else the option text.
    pub fn chosen_value(&self) -> Option<&str> {
        self.values
            .get(self.option)
            .map(String::as_str)
            .or_else(|| self.chosen())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKind {
    RemoteStart,
    RemoteStop,
    Reset,
    ChangeConfiguration,
    GetConfiguration,
    Trigger,
    Unlock,
    Availability,
    SetProfile,
    DataTransfer,
    GetLog,
    InstallCertificate,
    DeleteCertificate,
    GetInstalledCertificateIds,
    ExtendedTrigger,
    SignedFirmware,
    CertificateSigned,
    ClearChargingProfile,
    GetCompositeSchedule,
    ReserveNow,
    UpdateFirmware,
    GetDiagnostics,
    SendLocalList,
    Raw,
    /// A form generated from the registry spec of one action ([`Form::from_spec`]).
    Spec,
    Rule,
    Fault,
    RemoveRule,
}

/// How an injected fault answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    CallError,
    Silence,
    Close,
    InvalidPayload,
}

impl FaultKind {
    pub const ALL: [Self; 4] = [
        Self::CallError,
        Self::Silence,
        Self::Close,
        Self::InvalidPayload,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::CallError => "call_error",
            Self::Silence => "silence",
            Self::Close => "close",
            Self::InvalidPayload => "invalid_payload",
        }
    }
}

/// What a valid form produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Submitted {
    Send(Command),
    Rule {
        action: String,
        payload: Value,
        /// The one station it answers; `None` answers every station.
        station: Option<String>,
        /// The subprotocol it is limited to (`ocpp2.0.1`); `None` for every protocol.
        protocol: Option<String>,
    },
    Fault {
        action: String,
        kind: FaultKind,
        times: usize,
        payload: Value,
        /// The one station it applies to; `None` applies to every station.
        station: Option<String>,
        /// The subprotocol it is limited to; `None` for every protocol.
        protocol: Option<String>,
    },
    RemoveRule(String),
    /// Remove a rule of one station (`None`: a global rule limited to a protocol).
    RemoveScopedRule {
        station: Option<String>,
        action: String,
        protocol: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormEvent {
    None,
    Cancel,
    Submit,
}

/// A transaction a `RemoteStopTransaction` can target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopChoice {
    pub id: i32,
    pub connector: u32,
    pub id_tag: String,
}

/// A running OCPP 2.0.1 transaction a `RequestStopTransaction` can target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxChoice {
    /// The station-made transaction id (a string of at most 36 characters).
    pub id: String,
    pub evse: Option<u32>,
    pub connector: Option<u32>,
    pub id_token: Option<String>,
}

/// What a registry form needs to know besides the spec: the protocol (its schemas check the
/// payload), a fresh request id and the running transactions of the station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecContext {
    pub protocol: Protocol,
    /// The suggestion for `remoteStartId` and `requestId`.
    pub request_id: i32,
    pub transactions: Vec<TxChoice>,
}

impl SpecContext {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            request_id: 1,
            transactions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub kind: FormKind,
    pub title: String,
    /// The station the form acts on (empty for server-wide forms).
    pub station: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    /// A problem with the whole form (the request violates its schema).
    pub error: Option<String>,
    /// Transaction ids behind the options of a `RemoteStop` form.
    tx_ids: Vec<i32>,
    /// Behind the options of a `RemoveRule` form: the scoped rule (a station's or a
    /// protocol's) an option stands for, `None` for a global rule.
    targets: Vec<Option<ScopedRule>>,
    /// The protocol whose schemas check the payload of a `Raw` or `Spec` form.
    protocol: Protocol,
    /// The registry entry a `Spec` form was generated from.
    spec: Option<&'static registry::ActionSpec>,
    /// Leading fields in front of the ones the kind describes: 1 in a rule or fault form that
    /// has a `protocol` select first, else 0.
    lead: usize,
    /// The payload a `Raw` form last prefilled for its action (empty when none); another
    /// action's template replaces it only while the payload still equals it.
    template: String,
}

fn names<I, S>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    items.into_iter().map(|s| s.as_ref().to_owned()).collect()
}

/// The actions a central system may send (`DataTransfer` included), including the 7
/// Security Whitepaper actions (which also have typed forms), sorted by name. They come from
/// the action registry of the protocol the palette serves.
pub fn csms_actions() -> Vec<String> {
    csms_actions_of(ACTIONS_PROTOCOL)
}

/// The same for the table of `protocol`; empty for a subprotocol without one.
fn csms_actions_of(protocol: &str) -> Vec<String> {
    let mut names: Vec<String> = registry::protocol_actions(protocol)
        .map(|table| {
            table
                .csms_actions
                .iter()
                .map(|spec| spec.name.to_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The actions a station sends, which rules and faults can target.
fn station_actions() -> &'static [&'static str] {
    registry::protocol_actions(ACTIONS_PROTOCOL).map_or(&[], |table| table.station_actions)
}

/// The "no protocol" choice of the `protocol` select of a rule or fault form: the rule applies
/// to every protocol (the engine-wide behavior).
pub const ALL_PROTOCOLS: &str = "(all)";

/// The actions a rule or fault can target for `protocol` (one of the protocols with a table, or
/// [`ALL_PROTOCOLS`]: every action any table lists, sorted).
fn station_actions_for(protocol: &str) -> Vec<String> {
    if let Some(table) = registry::protocol_actions(protocol) {
        return names(table.station_actions);
    }
    let mut all: Vec<String> = registry::protocols()
        .filter_map(registry::protocol_actions)
        .flat_map(|table| names(table.station_actions))
        .collect();
    all.sort();
    all.dedup();
    all
}

impl Form {
    fn build(kind: FormKind, title: &str, station: &str, fields: Vec<Field>) -> Self {
        let mut form = Self {
            kind,
            title: title.to_owned(),
            station: station.to_owned(),
            fields,
            focus: 0,
            error: None,
            tx_ids: Vec::new(),
            targets: Vec::new(),
            protocol: Protocol::V16,
            spec: None,
            lead: 0,
            template: String::new(),
        };
        form.validate_all();
        form.error = None;
        form.focus = form.first_visible();
        form
    }

    /// The first field that is not hidden (0 when every field is).
    fn first_visible(&self) -> usize {
        self.fields.iter().position(|f| !f.is_hidden()).unwrap_or(0)
    }

    /// The visible field `step` places from the focus (forward when `step` is 1, backward
    /// when it is -1), wrapping around.
    fn next_visible(&self, step: isize) -> usize {
        let n = self.fields.len().max(1);
        let mut at = self.focus;
        for _ in 0..n {
            at = (at + n).wrapping_add_signed(step) % n;
            if self.fields.get(at).is_some_and(|f| !f.is_hidden()) {
                return at;
            }
        }
        self.focus
    }

    pub fn remote_start(station: &str) -> Self {
        Self::build(
            FormKind::RemoteStart,
            "RemoteStartTransaction",
            station,
            vec![
                Field::new("idTag", FieldKind::IdTag, ""),
                Field::new(
                    "connector (optional)",
                    FieldKind::Connector {
                        min: 1,
                        optional: true,
                    },
                    "",
                ),
            ],
        )
    }

    /// `choices` are the station's active transactions; the first is preselected.
    pub fn remote_stop(station: &str, choices: &[StopChoice]) -> Self {
        let options = choices
            .iter()
            .map(|c| {
                format!(
                    "{} (connector {}, idTag {})",
                    c.id,
                    c.connector,
                    kirby_ocpp_tui::text::sanitize(&c.id_tag)
                )
            })
            .collect();
        let mut form = Self::build(
            FormKind::RemoteStop,
            "RemoteStopTransaction",
            station,
            vec![Field::select("transaction", options)],
        );
        form.tx_ids = choices.iter().map(|c| c.id).collect();
        // The one running transaction is the target: keep it on screen.
        for field in &mut form.fields {
            field.keep_single = true;
        }
        form
    }

    pub fn reset(station: &str) -> Self {
        let mut kind = Field::select("type", names(ResetType::ALL.iter().map(|t| t.as_str())));
        // The safer one is preselected whatever the order of the specification.
        kind.option = kind.options.iter().position(|o| o == "Soft").unwrap_or(0);
        Self::build(FormKind::Reset, "Reset", station, vec![kind])
    }

    pub fn change_configuration(station: &str) -> Self {
        Self::build(
            FormKind::ChangeConfiguration,
            "ChangeConfiguration",
            station,
            vec![
                Field::new("key", FieldKind::Text, ""),
                Field::new("value", FieldKind::OptionalText, ""),
            ],
        )
    }

    pub fn get_configuration(station: &str) -> Self {
        Self::build(
            FormKind::GetConfiguration,
            "GetConfiguration",
            station,
            vec![Field::new(
                "keys (optional, empty = all)",
                FieldKind::KeyList,
                "",
            )],
        )
    }

    pub fn trigger(station: &str) -> Self {
        Self::build(
            FormKind::Trigger,
            "TriggerMessage",
            station,
            vec![
                Field::select(
                    "message",
                    names(MessageTrigger::ALL.iter().map(|t| t.as_str())),
                ),
                Field::new(
                    "connector (optional)",
                    FieldKind::Connector {
                        min: 1,
                        optional: true,
                    },
                    "",
                ),
            ],
        )
    }

    pub fn unlock(station: &str, connector: Option<u32>) -> Self {
        Self::build(
            FormKind::Unlock,
            "UnlockConnector",
            station,
            vec![Field::new(
                "connector",
                FieldKind::Connector {
                    min: 1,
                    optional: false,
                },
                &connector.unwrap_or(1).to_string(),
            )],
        )
    }

    pub fn availability(station: &str) -> Self {
        Self::build(
            FormKind::Availability,
            "ChangeAvailability",
            station,
            vec![
                Field::new(
                    "connector (0 = station)",
                    FieldKind::Connector {
                        min: 0,
                        optional: false,
                    },
                    "0",
                ),
                Field::select(
                    "type",
                    names(AvailabilityType::ALL.iter().map(|t| t.as_str())),
                ),
            ],
        )
    }

    pub fn set_profile(station: &str) -> Self {
        Self::build(
            FormKind::SetProfile,
            "SetChargingProfile",
            station,
            vec![
                Field::new(
                    "connector (0 = station)",
                    FieldKind::Connector {
                        min: 0,
                        optional: false,
                    },
                    "0",
                ),
                Field::new(
                    "csChargingProfiles (JSON)",
                    FieldKind::ProfileJson,
                    PROFILE_TEMPLATE,
                ),
            ],
        )
    }

    pub fn data_transfer(station: &str) -> Self {
        Self::build(
            FormKind::DataTransfer,
            "DataTransfer",
            station,
            vec![
                Field::new("vendorId", FieldKind::Text, ""),
                Field::new("messageId (optional)", FieldKind::OptionalText, ""),
                Field::new("data (optional)", FieldKind::OptionalText, ""),
            ],
        )
    }

    /// `request_id` is the suggested id of the log request.
    pub fn get_log(station: &str, request_id: i32) -> Self {
        Self::build(
            FormKind::GetLog,
            "GetLog",
            station,
            vec![
                Field::select("logType", names(LogType::ALL.iter().map(|t| t.as_str()))),
                Field::new(
                    "requestId",
                    FieldKind::Count { optional: false },
                    &request_id.to_string(),
                ),
                Field::new("remoteLocation (upload URL)", FieldKind::Text, ""),
                Field::new(
                    "retries (optional)",
                    FieldKind::Count { optional: true },
                    "",
                ),
                Field::new(
                    "retryInterval s (optional)",
                    FieldKind::Count { optional: true },
                    "",
                ),
                Field::new(
                    "oldestTimestamp (optional)",
                    FieldKind::Timestamp { optional: true },
                    "",
                ),
                Field::new(
                    "latestTimestamp (optional)",
                    FieldKind::Timestamp { optional: true },
                    "",
                ),
            ],
        )
    }

    pub fn install_certificate(station: &str) -> Self {
        Self::build(
            FormKind::InstallCertificate,
            "InstallCertificate",
            station,
            vec![
                Field::select(
                    "certificateType",
                    names(CertificateUse::ALL.iter().map(|t| t.as_str())),
                ),
                Field::new(
                    "certificate (PEM, paste)",
                    FieldKind::Pem {
                        max: MAX_CERTIFICATE_LEN,
                    },
                    "",
                ),
            ],
        )
    }

    pub fn delete_certificate(station: &str) -> Self {
        Self::build(
            FormKind::DeleteCertificate,
            "DeleteCertificate",
            station,
            vec![
                Field::select(
                    "hashAlgorithm",
                    names(HashAlgorithm::ALL.iter().map(|t| t.as_str())),
                ),
                Field::new("issuerNameHash (hex)", FieldKind::Text, ""),
                Field::new("issuerKeyHash (hex)", FieldKind::Text, ""),
                Field::new("serialNumber (hex)", FieldKind::Text, ""),
            ],
        )
    }

    pub fn get_installed_certificate_ids(station: &str) -> Self {
        Self::build(
            FormKind::GetInstalledCertificateIds,
            "GetInstalledCertificateIds",
            station,
            vec![Field::select(
                "certificateType",
                names(CertificateUse::ALL.iter().map(|t| t.as_str())),
            )],
        )
    }

    pub fn extended_trigger(station: &str) -> Self {
        let mut message = Field::select(
            "message",
            names(ExtendedMessageTrigger::ALL.iter().map(|t| t.as_str())),
        );
        message.option = message
            .options
            .iter()
            .position(|o| o == "SignChargePointCertificate")
            .unwrap_or(0);
        Self::build(
            FormKind::ExtendedTrigger,
            "ExtendedTriggerMessage",
            station,
            vec![
                message,
                Field::new(
                    "connector (optional)",
                    FieldKind::Connector {
                        min: 1,
                        optional: true,
                    },
                    "",
                ),
            ],
        )
    }

    /// `request_id` is the suggested id; the retrieve time defaults to `now`.
    pub fn signed_update_firmware(station: &str, request_id: i32, now: DateTime<Utc>) -> Self {
        Self::build(
            FormKind::SignedFirmware,
            "SignedUpdateFirmware",
            station,
            vec![
                Field::new("location (firmware URL)", FieldKind::Text, ""),
                Field::new(
                    "retrieveDateTime",
                    FieldKind::Timestamp { optional: false },
                    &now.to_rfc3339_opts(SecondsFormat::Secs, true),
                ),
                Field::new(
                    "requestId",
                    FieldKind::Count { optional: false },
                    &request_id.to_string(),
                ),
                Field::new(
                    "retries (optional)",
                    FieldKind::Count { optional: true },
                    "",
                ),
                Field::new(
                    "retryInterval s (optional)",
                    FieldKind::Count { optional: true },
                    "",
                ),
                Field::new(
                    "signingCertificate (PEM, paste)",
                    FieldKind::Pem {
                        max: MAX_CERTIFICATE_LEN,
                    },
                    "",
                ),
                Field::new("signature (base64)", FieldKind::Text, ""),
                Field::new(
                    "installDateTime (optional)",
                    FieldKind::Timestamp { optional: true },
                    "",
                ),
            ],
        )
    }

    pub fn certificate_signed(station: &str) -> Self {
        Self::build(
            FormKind::CertificateSigned,
            "CertificateSigned",
            station,
            vec![Field::new(
                "certificateChain (PEM, paste)",
                FieldKind::Pem {
                    max: MAX_CERTIFICATE_CHAIN_LEN,
                },
                "",
            )],
        )
    }

    pub fn clear_charging_profile(station: &str) -> Self {
        let mut purposes = vec![ANY.to_owned()];
        purposes.extend(names(
            ChargingProfilePurposeType::ALL.iter().map(|t| t.as_str()),
        ));
        Self::build(
            FormKind::ClearChargingProfile,
            "ClearChargingProfile",
            station,
            vec![
                Field::new("id (optional)", FieldKind::Count { optional: true }, ""),
                Field::new(
                    "connector (optional, 0 = station)",
                    FieldKind::Connector {
                        min: 0,
                        optional: true,
                    },
                    "",
                ),
                Field::select("chargingProfilePurpose", purposes),
                Field::new(
                    "stackLevel (optional)",
                    FieldKind::Count { optional: true },
                    "",
                ),
            ],
        )
    }

    pub fn get_composite_schedule(station: &str) -> Self {
        let mut units = vec![ANY.to_owned()];
        units.extend(names(ChargingRateUnitType::ALL.iter().map(|t| t.as_str())));
        Self::build(
            FormKind::GetCompositeSchedule,
            "GetCompositeSchedule",
            station,
            vec![
                Field::new(
                    "connector (0 = station)",
                    FieldKind::Connector {
                        min: 0,
                        optional: false,
                    },
                    "1",
                ),
                Field::new("duration s", FieldKind::Count { optional: false }, "3600"),
                Field::select("chargingRateUnit", units),
            ],
        )
    }

    /// `reservation_id` is the suggested id; the reservation expires an hour after `now`.
    pub fn reserve_now(station: &str, reservation_id: i32, now: DateTime<Utc>) -> Self {
        Self::build(
            FormKind::ReserveNow,
            "ReserveNow",
            station,
            vec![
                Field::new(
                    "connector (0 = any)",
                    FieldKind::Connector {
                        min: 0,
                        optional: false,
                    },
                    "1",
                ),
                Field::new(
                    "expiryDate",
                    FieldKind::Timestamp { optional: false },
                    &(now + chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
                ),
                Field::new("idTag", FieldKind::IdTag, ""),
                Field::new("parentIdTag (optional)", FieldKind::OptionalIdTag, ""),
                Field::new(
                    "reservationId",
                    FieldKind::Count { optional: false },
                    &reservation_id.to_string(),
                ),
            ],
        )
    }

    /// The retrieve time defaults to `now`.
    pub fn update_firmware(station: &str, now: DateTime<Utc>) -> Self {
        Self::build(
            FormKind::UpdateFirmware,
            "UpdateFirmware",
            station,
            vec![
                Field::new("location (firmware URL)", FieldKind::Text, ""),
                Field::new(
                    "retrieveDate",
                    FieldKind::Timestamp { optional: false },
                    &now.to_rfc3339_opts(SecondsFormat::Secs, true),
                ),
                Field::new(
                    "retries (optional)",
                    FieldKind::Count { optional: true },
                    "",
                ),
                Field::new(
                    "retryInterval s (optional)",
                    FieldKind::Count { optional: true },
                    "",
                ),
            ],
        )
    }

    pub fn get_diagnostics(station: &str) -> Self {
        Self::build(
            FormKind::GetDiagnostics,
            "GetDiagnostics",
            station,
            vec![
                Field::new("location (upload URL)", FieldKind::Text, ""),
                Field::new(
                    "retries (optional)",
                    FieldKind::Count { optional: true },
                    "",
                ),
                Field::new(
                    "retryInterval s (optional)",
                    FieldKind::Count { optional: true },
                    "",
                ),
                Field::new(
                    "startTime (optional)",
                    FieldKind::Timestamp { optional: true },
                    "",
                ),
                Field::new(
                    "stopTime (optional)",
                    FieldKind::Timestamp { optional: true },
                    "",
                ),
            ],
        )
    }

    pub fn send_local_list(station: &str) -> Self {
        Self::build(
            FormKind::SendLocalList,
            "SendLocalList",
            station,
            vec![
                Field::select(
                    "updateType",
                    names(UpdateType::ALL.iter().map(|t| t.as_str())),
                ),
                Field::new("listVersion", FieldKind::Count { optional: false }, "1"),
                Field::new(
                    "entries: idTag[:status], comma separated (:- deletes)",
                    FieldKind::AuthList,
                    "",
                ),
            ],
        )
    }

    pub fn raw(station: &str, initial_action: Option<&str>) -> Self {
        Self::raw_in(station, ACTIONS_PROTOCOL, initial_action)
    }

    /// The raw call form of `protocol`: its actions, its schemas.
    pub fn raw_in(station: &str, protocol: &str, initial_action: Option<&str>) -> Self {
        let mut action = Field::select("action", csms_actions_of(protocol));
        if let Some(i) = initial_action.and_then(|a| action.options.iter().position(|o| o == a)) {
            action.option = i;
        }
        let template = action
            .chosen()
            .map(|name| payload_template_text(protocol, name))
            .unwrap_or_default();
        let mut form = Self::build(
            FormKind::Raw,
            "Raw CALL",
            station,
            vec![action, Field::raw_payload(&template)],
        )
        .in_protocol(Protocol::from_token(protocol).unwrap_or(Protocol::V16));
        form.template = template;
        form
    }

    /// Checks the payload against the schemas of `protocol` from now on.
    fn in_protocol(mut self, protocol: Protocol) -> Self {
        self.protocol = protocol;
        self.validate_all();
        self.error = None;
        self.quiet_blank_payload();
        self
    }

    /// An untouched empty payload shows its hint, not the schema violation of `{}`; the error
    /// comes back once the payload is edited or the form is submitted.
    fn quiet_blank_payload(&mut self) {
        if self.kind == FormKind::Raw
            && let Some(f) = self.fields.get_mut(1)
            && f.kind == FieldKind::RawJson
            && f.input.text().trim().is_empty()
        {
            f.error = None;
        }
    }

    /// The generic form of a registry action of the station's protocol: one field per
    /// `FieldSpec` (dotted names build nested objects, empty optional fields are left out), or
    /// a JSON payload field with the action fixed when the spec has no fields. The whole
    /// request is checked against the schemas of `ctx.protocol` while typing. It submits as
    /// [`Command::Raw`].
    pub fn from_spec(
        station: &str,
        spec: &'static registry::ActionSpec,
        ctx: &SpecContext,
    ) -> Self {
        if spec.fields.is_empty() {
            let template = payload_template_text(ctx.protocol.token(), spec.name);
            let mut form = Self::build(
                FormKind::Raw,
                spec.title,
                station,
                vec![
                    Field::select("action", vec![spec.name.to_owned()]),
                    Field::raw_payload(&template),
                ],
            )
            .in_protocol(ctx.protocol);
            form.template = template;
            return form;
        }
        let fields = spec.fields.iter().map(|f| spec_field(f, ctx)).collect();
        let mut form = Self::build(FormKind::Spec, spec.title, station, fields);
        form.spec = Some(spec);
        form.protocol = ctx.protocol;
        form.check_request();
        form.error = None;
        form
    }

    /// The registry entry a `Spec` form was generated from.
    pub fn spec(&self) -> Option<&'static registry::ActionSpec> {
        self.spec
    }

    /// The payload a `Spec` form would send now (without checking it).
    pub fn spec_payload(&self) -> Option<Value> {
        let mut root = serde_json::Map::new();
        for f in &self.fields {
            let Some(spec) = f.spec else { continue };
            let value = if f.kind == FieldKind::Select {
                match f.chosen_value() {
                    Some(v) if v != ANY => Value::String(v.to_owned()),
                    _ => continue,
                }
            } else {
                let text = f.input.text().trim();
                if text.is_empty() {
                    continue;
                }
                match spec.ty {
                    registry::FieldType::Integer => Value::from(text.parse::<i32>().ok()?),
                    registry::FieldType::Json => json_value(text).ok()?,
                    registry::FieldType::Timestamp => Value::String(
                        DateTime::parse_from_rfc3339(text)
                            .ok()?
                            .with_timezone(&Utc)
                            .to_rfc3339_opts(SecondsFormat::Secs, true),
                    ),
                    registry::FieldType::Pem => {
                        Value::String(normalize_pem(text, MAX_CERTIFICATE_CHAIN_LEN).ok()?)
                    }
                    _ => Value::String(text.to_owned()),
                }
            };
            insert_path(&mut root, spec.name, value);
        }
        Some(Value::Object(root))
    }

    /// Why the whole request of a `Spec` form violates the schema, if it does.
    fn spec_schema_error(&self) -> Option<String> {
        let spec = self.spec?;
        let payload = self.spec_payload()?;
        match SchemaRegistry::for_protocol(self.protocol) {
            Ok(schemas) => schemas
                .validate_request(spec.name, &payload)
                .err()
                .map(|e| e.to_string()),
            Err(e) => Some(e.to_string()),
        }
    }

    /// Sets the form error from the schema once every field is fine on its own.
    fn check_request(&mut self) {
        if self.kind == FormKind::Spec && self.fields.iter().all(|f| f.error.is_none()) {
            self.error = self.spec_schema_error();
        }
    }

    /// The `protocol` select of a rule or fault form: the protocols that have an action table,
    /// then [`ALL_PROTOCOLS`]; `default` is preselected when it is one of them.
    fn protocol_select(default: &str) -> Field {
        let mut options = names(registry::protocols());
        options.push(ALL_PROTOCOLS.to_owned());
        let mut field = Field::select("protocol", options);
        field.option = field.options.iter().position(|o| o == default).unwrap_or(0);
        field
    }

    /// Re-lists the actions of a rule or fault form after its protocol changed, keeping the
    /// chosen action when the new list has it.
    fn refresh_actions(&mut self) {
        let protocol = self
            .fields
            .first()
            .and_then(Field::chosen)
            .unwrap_or_default()
            .to_owned();
        let list = station_actions_for(&protocol);
        if let Some(action) = self.fields.get_mut(1) {
            let current = action.chosen().map(str::to_owned);
            action.option = current
                .and_then(|c| list.iter().position(|a| *a == c))
                .unwrap_or(0);
            action.options = list;
        }
    }

    /// A response rule form with a `protocol` select first (`default` is preselected).
    pub fn rule_in(default: &str) -> Self {
        let protocol = Self::protocol_select(default);
        let actions = station_actions_for(protocol.chosen().unwrap_or_default());
        let mut form = Self::build(
            FormKind::Rule,
            "Response rule (all stations, or one if set)",
            "",
            vec![
                protocol,
                Field::select("action", actions),
                Field::new("CALLRESULT payload (JSON)", FieldKind::Json, "{}"),
                Field::new(STATION_LABEL, FieldKind::Station, ""),
            ],
        );
        form.lead = 1;
        form.validate_all();
        form.error = None;
        form
    }

    /// An inject-fault form with a `protocol` select first (`default` is preselected).
    pub fn fault_in(default: &str) -> Self {
        let protocol = Self::protocol_select(default);
        let actions = station_actions_for(protocol.chosen().unwrap_or_default());
        let mut form = Self::build(
            FormKind::Fault,
            "Inject fault (all stations, or one if set)",
            "",
            vec![
                protocol,
                Field::select("action", actions),
                Field::select("kind", names(FaultKind::ALL.iter().map(|k| k.name()))),
                Field::new("times", FieldKind::Times, "1"),
                Field::new("payload (invalid_payload only)", FieldKind::FaultJson, "{}"),
                Field::new(STATION_LABEL, FieldKind::Station, ""),
            ],
        );
        form.lead = 1;
        form.validate_all();
        form.error = None;
        form
    }

    pub fn rule() -> Self {
        Self::build(
            FormKind::Rule,
            "Response rule (all stations, or one if set)",
            "",
            vec![
                Field::select("action", names(station_actions())),
                Field::new(
                    "CALLRESULT payload (JSON)",
                    FieldKind::Json,
                    r#"{"status":"Rejected"}"#,
                ),
                Field::new(STATION_LABEL, FieldKind::Station, ""),
            ],
        )
    }

    pub fn fault() -> Self {
        Self::build(
            FormKind::Fault,
            "Inject fault (all stations, or one if set)",
            "",
            vec![
                Field::select("action", names(station_actions())),
                Field::select("kind", names(FaultKind::ALL.iter().map(|k| k.name()))),
                Field::new("times", FieldKind::Times, "1"),
                Field::new("payload (invalid_payload only)", FieldKind::FaultJson, "{}"),
                Field::new(STATION_LABEL, FieldKind::Station, ""),
            ],
        )
    }

    /// `actions` are the ones that have a global rule.
    pub fn remove_rule(actions: &[String]) -> Self {
        Self::remove_rule_scoped(actions, &[])
    }

    /// `actions` are the ones that have a global rule, `scoped` the rules of one station (or
    /// of one protocol); the options list the global rules first.
    pub fn remove_rule_scoped(actions: &[String], scoped: &[ScopedRule]) -> Self {
        let mut options = actions.to_vec();
        options.extend(scoped.iter().map(|rule| {
            let label = rule.label();
            kirby_ocpp_tui::text::sanitize(&label)
        }));
        let mut form = Self::build(
            FormKind::RemoveRule,
            "Remove response rule",
            "",
            vec![Field::select("action", options)],
        );
        form.targets = actions
            .iter()
            .map(|_| None)
            .chain(scoped.iter().cloned().map(Some))
            .collect();
        // The one rule is the target: keep it on screen.
        for field in &mut form.fields {
            field.keep_single = true;
        }
        form
    }

    pub fn paste(&mut self, text: &str) {
        let i = self.focus;
        if let Some(f) = self.fields.get_mut(i)
            && f.is_text()
        {
            f.capped |= insert_str_capped(&mut f.input, text);
            self.touched(i);
        }
    }

    fn touched(&mut self, i: usize) {
        if let Some(f) = self.fields.get_mut(i) {
            f.touched = true;
        }
        self.validate_field(i);
        self.error = None;
        self.after_edit();
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormEvent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return FormEvent::Cancel,
            KeyCode::Enter => return FormEvent::Submit,
            KeyCode::Tab | KeyCode::Down => self.focus = self.next_visible(1),
            KeyCode::BackTab | KeyCode::Up => self.focus = self.next_visible(-1),
            code => {
                let i = self.focus;
                let Some(f) = self.fields.get_mut(i) else {
                    return FormEvent::None;
                };
                match (f.kind, code) {
                    (FieldKind::Select, KeyCode::Left) => step(&mut f.option, f.options.len(), -1),
                    (FieldKind::Select, KeyCode::Right) => step(&mut f.option, f.options.len(), 1),
                    (FieldKind::Select, KeyCode::Char(c)) if !ctrl => {
                        let lc = c.to_ascii_lowercase();
                        let len = f.options.len();
                        if let Some(next) = (1..=len).map(|d| (f.option + d) % len).find(|&k| {
                            f.options.get(k).is_some_and(|o| {
                                o.chars()
                                    .next()
                                    .is_some_and(|c| c.to_ascii_lowercase() == lc)
                            })
                        }) {
                            f.option = next;
                        }
                    }
                    (FieldKind::Select, _) => {}
                    (_, KeyCode::Left) => f.input.left(),
                    (_, KeyCode::Right) => f.input.right(),
                    (_, KeyCode::Home) => f.input.home(),
                    (_, KeyCode::End) => f.input.end(),
                    (_, KeyCode::Backspace) => f.input.backspace(),
                    (_, KeyCode::Delete) => f.input.delete(),
                    (_, KeyCode::Char('a')) if ctrl => f.input.home(),
                    (_, KeyCode::Char('e')) if ctrl => f.input.end(),
                    (_, KeyCode::Char('u')) if ctrl => f.input = TextInput::default(),
                    (_, KeyCode::Char(c))
                        if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        f.capped |= insert_char_capped(&mut f.input, c);
                    }
                    _ => {}
                }
                self.touched(i);
            }
        }
        FormEvent::None
    }

    /// The field whose check depends on another one.
    fn dependent(&self) -> Option<usize> {
        match self.kind {
            FormKind::Raw | FormKind::SetProfile => Some(1),
            FormKind::Fault => Some(self.lead + 3),
            _ => None,
        }
    }

    fn after_edit(&mut self) {
        if let Some(i) = self.dependent() {
            self.validate_field(i);
        }
        if self.lead == 1 && self.focus == 0 {
            self.refresh_actions();
        }
        if self.focus == 0 {
            self.swap_template();
            // Changing the action does not edit the payload.
            self.quiet_blank_payload();
        }
        self.check_request();
    }

    /// A raw form whose action changed starts the payload over with the template of the new
    /// action, unless the payload was edited.
    fn swap_template(&mut self) {
        if self.kind != FormKind::Raw {
            return;
        }
        let Some(action) = self
            .fields
            .first()
            .and_then(Field::chosen)
            .map(str::to_owned)
        else {
            return;
        };
        let protocol = self.protocol.token();
        let next = payload_template_text(protocol, &action);
        let Some(payload) = self.fields.get_mut(1) else {
            return;
        };
        let current = payload.input.text();
        if next != self.template && (current.trim().is_empty() || current == self.template) {
            payload.input = TextInput::new(next.clone());
            payload.touched = false;
            self.template = next;
            self.validate_field(1);
        }
    }

    pub fn validate_all(&mut self) -> bool {
        for i in 0..self.fields.len() {
            self.validate_field(i);
        }
        self.fields.iter().all(|f| f.error.is_none())
    }

    fn text(&self, i: usize) -> String {
        self.fields
            .get(i)
            .map(|f| f.input.text().trim().to_owned())
            .unwrap_or_default()
    }

    /// The field as typed, spaces at the ends included (free-text values).
    fn untrimmed(&self, i: usize) -> String {
        self.fields
            .get(i)
            .map(|f| f.input.text().to_owned())
            .unwrap_or_default()
    }

    /// The normalised PEM of a `Pem` field.
    fn pem(&self, i: usize) -> Option<String> {
        let FieldKind::Pem { max } = self.fields.get(i)?.kind else {
            return None;
        };
        normalize_pem(&self.text(i), max).ok()
    }

    fn validate_field(&mut self, i: usize) {
        let action = self
            .fields
            .get(self.lead)
            .and_then(|f| f.chosen())
            .map(str::to_owned);
        let connector_text = self.text(0);
        let fault_kind = self
            .fields
            .get(self.lead + 1)
            .and_then(|f| f.chosen())
            .map(str::to_owned);
        let form_kind = self.kind;
        let protocol = self.protocol;
        let Some(f) = self.fields.get_mut(i) else {
            return;
        };
        let text = f.input.text().trim().to_owned();
        f.error = match f.kind {
            FieldKind::Text => text.is_empty().then(|| "required".to_owned()),
            FieldKind::OptionalText => None,
            FieldKind::Select => (f.spec.is_some() && f.options.is_empty())
                .then(|| "nothing to choose from".to_owned()),
            FieldKind::Spec => f
                .spec
                .and_then(|spec| spec_text_error(spec, f.input.text())),
            FieldKind::IdTag => {
                if text.is_empty() {
                    Some("idTag is required".to_owned())
                } else {
                    IdTag::new(text).err().map(|e| e.to_string())
                }
            }
            FieldKind::OptionalIdTag => {
                if text.is_empty() {
                    None
                } else {
                    IdTag::new(text).err().map(|e| e.to_string())
                }
            }
            FieldKind::AuthList => parse_auth_list(&text).err(),
            FieldKind::Connector { min, optional } => {
                if text.is_empty() && optional {
                    None
                } else {
                    match text.parse::<u32>() {
                        Ok(n) if n >= min => None,
                        _ => Some(format!("connector must be a whole number, {min} or more")),
                    }
                }
            }
            FieldKind::Times => match text.parse::<usize>() {
                Ok(n) if (1..=TIMES_MAX).contains(&n) => None,
                _ => Some(format!("a number from 1 to {TIMES_MAX}")),
            },
            FieldKind::Json => json_object(f.input.text()).err(),
            FieldKind::RawJson => validate_request(
                protocol,
                raw_payload_text(f.input.text()),
                action.as_deref().unwrap_or_default(),
            ),
            FieldKind::ProfileJson => validate_profile(f.input.text(), &connector_text),
            FieldKind::FaultJson => {
                if fault_kind.as_deref() == Some(FaultKind::InvalidPayload.name()) {
                    json_object(f.input.text()).err()
                } else {
                    None
                }
            }
            FieldKind::Station => station_error(&text),
            FieldKind::KeyList => validate_keys(&text),
            FieldKind::Pem { max } => normalize_pem(&text, max).err(),
            FieldKind::Count { optional } => {
                if text.is_empty() && optional {
                    None
                } else {
                    match text.parse::<i32>() {
                        Ok(n) if n >= 0 => None,
                        _ => Some("a whole number, 0 or more".to_owned()),
                    }
                }
            }
            FieldKind::Timestamp { optional } => {
                if text.is_empty() && optional {
                    None
                } else {
                    DateTime::parse_from_rfc3339(&text)
                        .err()
                        .map(|_| "a time like 2026-01-31T12:00:00Z".to_owned())
                }
            }
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

    /// The protocol a rule or fault form is limited to: `None` without a `protocol` select and
    /// for [`ALL_PROTOCOLS`].
    fn chosen_protocol(&self) -> Option<String> {
        if self.lead == 0 {
            return None;
        }
        self.fields
            .first()
            .and_then(Field::chosen)
            .filter(|p| *p != ALL_PROTOCOLS)
            .map(str::to_owned)
    }

    /// Whether field `i` holds a secret that is drawn as `*`: the value of a
    /// ChangeConfiguration whose key is a secret one (`AuthorizationKey`).
    pub fn is_secret(&self, i: usize) -> bool {
        self.kind == FormKind::ChangeConfiguration
            && i == 1
            && self
                .fields
                .first()
                .is_some_and(|k| is_secret_config_key(k.input.text()))
    }

    /// Validates and converts the form. On failure the errors are set on the fields (or on
    /// the form for a schema violation of the whole request).
    pub fn submit(&mut self) -> Option<Submitted> {
        self.error = None;
        if !self.validate_all() {
            // A submit makes every missing value an error, not a hint.
            for f in &mut self.fields {
                f.touched = true;
            }
            if let Some(i) = self.fields.iter().position(|f| f.error.is_some()) {
                self.focus = i;
            }
            return None;
        }
        let opt = |s: String| Some(s).filter(|s| !s.is_empty());
        let chosen = |i: usize| {
            self.fields
                .get(i)
                .and_then(|f| f.chosen())
                .map(str::to_owned)
        };
        let number = |i: usize| self.text(i).parse::<u32>().ok();
        let count = |i: usize| self.text(i).parse::<i32>().ok();
        let time = |i: usize| {
            DateTime::parse_from_rfc3339(&self.text(i))
                .ok()
                .map(|t| t.with_timezone(&Utc))
        };
        let submitted = match self.kind {
            FormKind::RemoteStart => Submitted::Send(Command::RemoteStart {
                id_tag: self.text(0),
                connector: number(1),
            }),
            FormKind::RemoteStop => {
                let at = self.fields.first()?.option;
                Submitted::Send(Command::RemoteStop {
                    transaction_id: *self.tx_ids.get(at)?,
                })
            }
            FormKind::Reset => Submitted::Send(Command::Reset(chosen(0)?.parse().ok()?)),
            FormKind::ChangeConfiguration => Submitted::Send(Command::ChangeConfiguration {
                key: self.text(0),
                // Free text: spaces at the ends are part of the value.
                value: self.untrimmed(1),
            }),
            FormKind::GetConfiguration => Submitted::Send(Command::GetConfiguration {
                keys: split_keys(&self.text(0)),
            }),
            FormKind::Trigger => Submitted::Send(Command::TriggerMessage {
                message: chosen(0)?.parse().ok()?,
                connector: number(1),
            }),
            FormKind::Unlock => Submitted::Send(Command::UnlockConnector {
                connector: number(0)?,
            }),
            FormKind::Availability => Submitted::Send(Command::ChangeAvailability {
                connector: number(0)?,
                kind: chosen(1)?.parse().ok()?,
            }),
            FormKind::SetProfile => Submitted::Send(Command::SetChargingProfile {
                connector: number(0)?,
                profile: Box::new(
                    serde_json::from_str::<ChargingProfile>(self.fields.get(1)?.input.text())
                        .ok()?,
                ),
            }),
            FormKind::DataTransfer => Submitted::Send(Command::DataTransfer {
                vendor_id: self.text(0),
                message_id: opt(self.text(1)),
                // Free text: spaces at the ends are part of the value.
                data: opt(self.untrimmed(2)).filter(|s| !s.trim().is_empty()),
            }),
            FormKind::GetLog => Submitted::Send(Command::GetLog(Box::new(GetLogRequest {
                log_type: chosen(0)?.parse().ok()?,
                request_id: count(1)?,
                retries: count(3),
                retry_interval: count(4),
                log: LogParameters {
                    remote_location: self.text(2),
                    oldest_timestamp: time(5),
                    latest_timestamp: time(6),
                },
            }))),
            FormKind::InstallCertificate => Submitted::Send(Command::InstallCertificate {
                certificate_type: chosen(0)?.parse().ok()?,
                certificate: self.pem(1)?,
            }),
            FormKind::DeleteCertificate => {
                Submitted::Send(Command::DeleteCertificate(CertificateHashData {
                    hash_algorithm: chosen(0)?.parse().ok()?,
                    issuer_name_hash: self.text(1),
                    issuer_key_hash: self.text(2),
                    serial_number: self.text(3),
                }))
            }
            FormKind::GetInstalledCertificateIds => Submitted::Send(
                Command::GetInstalledCertificateIds(chosen(0)?.parse().ok()?),
            ),
            FormKind::ExtendedTrigger => Submitted::Send(Command::ExtendedTriggerMessage {
                message: chosen(0)?.parse().ok()?,
                connector: number(1),
            }),
            FormKind::SignedFirmware => Submitted::Send(Command::SignedUpdateFirmware(Box::new(
                SignedUpdateFirmwareRequest {
                    retries: count(3),
                    retry_interval: count(4),
                    request_id: count(2)?,
                    firmware: SignedFirmware {
                        location: self.text(0),
                        retrieve_date_time: time(1)?,
                        install_date_time: time(7),
                        signing_certificate: self.pem(5)?,
                        signature: self.text(6),
                    },
                },
            ))),
            FormKind::CertificateSigned => Submitted::Send(Command::CertificateSigned {
                chain: self.pem(0)?,
            }),
            FormKind::ClearChargingProfile => {
                Submitted::Send(Command::ClearChargingProfile(ClearChargingProfileRequest {
                    id: count(0),
                    connector_id: number(1).map(ConnectorId::new),
                    charging_profile_purpose: chosen(2)
                        .filter(|p| p != ANY)
                        .and_then(|p| p.parse().ok()),
                    stack_level: count(3),
                }))
            }
            FormKind::GetCompositeSchedule => Submitted::Send(Command::GetCompositeSchedule {
                connector: number(0)?,
                duration: count(1)?,
                unit: chosen(2).filter(|u| u != ANY).and_then(|u| u.parse().ok()),
            }),
            FormKind::ReserveNow => {
                Submitted::Send(Command::ReserveNow(Box::new(ReserveNowRequest {
                    connector_id: ConnectorId::new(number(0)?),
                    expiry_date: time(1)?,
                    id_tag: IdTag::new(self.text(2)).ok()?,
                    parent_id_tag: opt(self.text(3)).and_then(|t| IdTag::new(t).ok()),
                    reservation_id: ReservationId::new(count(4)?),
                })))
            }
            FormKind::UpdateFirmware => {
                Submitted::Send(Command::UpdateFirmware(Box::new(UpdateFirmwareRequest {
                    location: self.text(0),
                    retrieve_date: time(1)?,
                    retries: count(2),
                    retry_interval: count(3),
                })))
            }
            FormKind::GetDiagnostics => {
                Submitted::Send(Command::GetDiagnostics(Box::new(GetDiagnosticsRequest {
                    location: self.text(0),
                    retries: count(1),
                    retry_interval: count(2),
                    start_time: time(3),
                    stop_time: time(4),
                })))
            }
            FormKind::SendLocalList => {
                let entries = parse_auth_list(&self.text(2)).ok()?;
                Submitted::Send(Command::SendLocalList(Box::new(SendLocalListRequest {
                    list_version: count(1)?,
                    local_authorization_list: (!entries.is_empty()).then_some(entries),
                    update_type: chosen(0)?.parse().ok()?,
                })))
            }
            FormKind::Raw => Submitted::Send(Command::Raw {
                action: chosen(0)?,
                payload: serde_json::from_str(raw_payload_text(self.fields.get(1)?.input.text()))
                    .ok()?,
            }),
            FormKind::Spec => {
                let spec = self.spec?;
                let payload = self.spec_payload()?;
                if let Some(error) = self.spec_schema_error() {
                    self.error = Some(error);
                    return None;
                }
                Submitted::Send(Command::Raw {
                    action: spec.name.to_owned(),
                    payload,
                })
            }
            FormKind::Rule => {
                let at = self.lead;
                Submitted::Rule {
                    action: chosen(at)?,
                    payload: serde_json::from_str(self.fields.get(at + 1)?.input.text()).ok()?,
                    station: Some(self.text(at + 2)).filter(|station| !station.is_empty()),
                    protocol: self.chosen_protocol(),
                }
            }
            FormKind::Fault => {
                let at = self.lead;
                let kind = FaultKind::ALL
                    .iter()
                    .copied()
                    .find(|k| Some(k.name()) == chosen(at + 1).as_deref())?;
                let payload = if kind == FaultKind::InvalidPayload {
                    serde_json::from_str(self.fields.get(at + 3)?.input.text()).ok()?
                } else {
                    Value::Null
                };
                Submitted::Fault {
                    action: chosen(at)?,
                    kind,
                    times: self.text(at + 2).parse().ok()?,
                    payload,
                    station: Some(self.text(at + 4)).filter(|station| !station.is_empty()),
                    protocol: self.chosen_protocol(),
                }
            }
            FormKind::RemoveRule => match self.targets.get(self.fields.first()?.option) {
                Some(Some(rule)) => Submitted::RemoveScopedRule {
                    station: rule.station.clone(),
                    action: rule.action.clone(),
                    protocol: rule.protocol.clone(),
                },
                Some(None) | None => Submitted::RemoveRule(chosen(0)?),
            },
        };
        if let Submitted::Send(command) = &submitted
            && !matches!(command, Command::Raw { .. })
            && let Err(e) = schemas().validate_request(command.action(), &command.payload())
        {
            self.error = Some(e.to_string());
            return None;
        }
        Some(submitted)
    }
}

/// Rebuilds PEM blocks from text whose line breaks may have become spaces (a paste into a
/// single-line field): each block gets its header, base64 lines of 64 characters and footer.
fn normalize_pem(text: &str, max: usize) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text.trim();
    if rest.is_empty() {
        return Err("required".to_owned());
    }
    while !rest.is_empty() {
        let after = rest
            .strip_prefix("-----BEGIN ")
            .ok_or_else(|| "expected a PEM block starting with -----BEGIN ...-----".to_owned())?;
        let (label, body_and_tail) = after
            .split_once("-----")
            .ok_or_else(|| "unterminated -----BEGIN line".to_owned())?;
        let footer = format!("-----END {label}-----");
        let (body, tail) = body_and_tail
            .split_once(footer.as_str())
            .ok_or_else(|| format!("missing {}", kirby_ocpp_tui::text::sanitize(&footer)))?;
        let data: String = body.split_whitespace().collect();
        if data.is_empty()
            || !data
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        {
            return Err("the PEM body is not base64".to_owned());
        }
        let _ = writeln!(out, "-----BEGIN {label}-----");
        for chunk in data.as_bytes().chunks(64) {
            out.push_str(&String::from_utf8_lossy(chunk));
            out.push('\n');
        }
        out.push_str(&footer);
        out.push('\n');
        rest = tail.trim_start();
    }
    if out.chars().count() > max {
        return Err(format!("longer than {max} characters"));
    }
    Ok(out)
}

fn step(i: &mut usize, n: usize, d: isize) {
    if n > 0 {
        *i = (*i + n).wrapping_add_signed(d) % n;
    }
}

/// Parses `idTag[:status]` items separated by commas. A missing status means Accepted, `-`
/// leaves out the `idTagInfo` (a differential update deletes such an entry).
fn parse_auth_list(text: &str) -> Result<Vec<AuthorizationData>, String> {
    let mut out = Vec::new();
    for item in text.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        if out.len() >= AUTH_LIST_MAX {
            return Err(format!("at most {AUTH_LIST_MAX} entries"));
        }
        let (tag, status) = item
            .split_once(':')
            .map_or((item, ""), |(t, s)| (t, s.trim()));
        let tag = tag.trim();
        let shown = truncate_key(item);
        if tag.is_empty() {
            return Err(format!("empty idTag in \"{shown}\""));
        }
        let id_tag = IdTag::new(tag).map_err(|e| format!("{e}: {shown}"))?;
        let id_tag_info = if status == "-" {
            None
        } else if status.is_empty() {
            Some(IdTagInfo::with_status(AuthorizationStatus::Accepted))
        } else {
            let found = AuthorizationStatus::ALL
                .iter()
                .find(|s| s.as_str().eq_ignore_ascii_case(status))
                .ok_or_else(|| {
                    let all: Vec<_> = AuthorizationStatus::ALL
                        .iter()
                        .map(|s| s.as_str())
                        .collect();
                    format!("unknown status in \"{shown}\"; use {}", all.join(", "))
                })?;
            Some(IdTagInfo::with_status(*found))
        };
        out.push(AuthorizationData {
            id_tag,
            id_tag_info,
        });
    }
    Ok(out)
}

/// The problem with a station id typed into a form; empty is fine (every station).
fn station_error(text: &str) -> Option<String> {
    if text.len() > MAX_STATION_ID_LEN {
        return Some(format!("at most {MAX_STATION_ID_LEN} bytes"));
    }
    text.chars()
        .any(kirby_ocpp_core::text::is_unsafe_char)
        .then(|| "no control characters".to_owned())
}

fn split_keys(text: &str) -> Vec<String> {
    text.split([',', ' '])
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
        .collect()
}

fn validate_keys(text: &str) -> Option<String> {
    split_keys(text)
        .iter()
        .find(|k| k.chars().count() > KEY_MAX)
        .map(|k| format!("a key is at most {KEY_MAX} characters: {}", truncate_key(k)))
}

fn truncate_key(k: &str) -> String {
    kirby_ocpp_tui::text::truncate(&kirby_ocpp_tui::text::sanitize(k), 20)
}

/// The minimal valid payload of `action` as the text a raw payload field starts with; empty
/// when none can be derived.
fn payload_template_text(protocol: &str, action: &str) -> String {
    registry::payload_template(protocol, action).map_or_else(String::new, |v| v.to_string())
}

/// The payload of a raw call form: an empty field means `{}`.
fn raw_payload_text(text: &str) -> &str {
    if text.trim().is_empty() { "{}" } else { text }
}

fn json_object(text: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err("must be a JSON object".to_owned())
    }
}

/// `None` when `text` is a JSON object that satisfies the request schema of `action` in
/// `protocol`.
fn validate_request(protocol: Protocol, text: &str, action: &str) -> Option<String> {
    let value = match json_object(text) {
        Ok(v) => v,
        Err(e) => return Some(e),
    };
    let registry = if protocol == Protocol::V16 {
        schemas()
    } else {
        match SchemaRegistry::for_protocol(protocol) {
            Ok(registry) => registry,
            Err(e) => return Some(e.to_string()),
        }
    };
    registry
        .validate_request(action, &value)
        .err()
        .map(|e| e.to_string())
}

/// The input of one registry field: a select for a choice, else a typed text input.
fn spec_field(spec: &'static registry::FieldSpec, ctx: &SpecContext) -> Field {
    let mut field = if spec.ty == registry::FieldType::Choice {
        let (mut options, mut values) = if spec.choices.is_empty() {
            transaction_options(&ctx.transactions)
        } else {
            (names(spec.choices), Vec::new())
        };
        if !spec.required {
            options.insert(0, ANY.to_owned());
            if !values.is_empty() {
                values.insert(0, ANY.to_owned());
            }
        }
        let mut field = Field::select(spec.label, options);
        field.values = values;
        if let Some(default) = spec.default {
            field.option = field.options.iter().position(|o| o == default).unwrap_or(0);
        }
        field
    } else {
        let default = match spec.name {
            // A fresh id per form, as the 1.6 forms take one from the session counter.
            "remoteStartId" | "requestId" => ctx.request_id.to_string(),
            _ => spec.default.unwrap_or_default().to_owned(),
        };
        Field::new(spec.label, FieldKind::Spec, &default)
    };
    field.hint = spec.hint;
    field.spec = Some(spec);
    field
}

/// The options of a transaction choice: `id8  evse/connector  idToken` and the full id behind
/// each. The text comes from the station, so it is sanitized and cut.
fn transaction_options(transactions: &[TxChoice]) -> (Vec<String>, Vec<String>) {
    use kirby_ocpp_tui::text::{sanitize, truncate};
    let part = |n: Option<u32>| n.map_or_else(|| "-".to_owned(), |n| n.to_string());
    transactions
        .iter()
        .map(|tx| {
            let short: String = sanitize(&tx.id).chars().take(8).collect();
            let token = tx.id_token.as_deref().map(sanitize).unwrap_or_default();
            let text = format!(
                "{short}  {}/{}  {}",
                part(tx.evse),
                part(tx.connector),
                truncate(&token, 36)
            );
            (text.trim_end().to_owned(), tx.id.clone())
        })
        .unzip()
}

/// Why `raw` is not an acceptable value of the registry field `spec`, or `None`.
fn spec_text_error(spec: &registry::FieldSpec, raw: &str) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() {
        return spec.required.then(|| "required".to_owned());
    }
    match spec.ty {
        registry::FieldType::Integer => (!matches!(text.parse::<i32>(), Ok(n) if n >= 0))
            .then(|| "a whole number, 0 or more".to_owned()),
        registry::FieldType::Timestamp => DateTime::parse_from_rfc3339(text)
            .err()
            .map(|_| "a time like 2026-01-31T12:00:00Z".to_owned()),
        registry::FieldType::Json => json_value(text).err(),
        registry::FieldType::Pem => normalize_pem(text, MAX_CERTIFICATE_CHAIN_LEN).err(),
        _ => None,
    }
}

/// Any JSON value. Text that is not JSON and does not start like an object, an array or a
/// string is taken as a plain string, so `hello` and `42` both work for a free `data` field;
/// the schema then decides whether the type fits.
fn json_value(text: &str) -> Result<Value, String> {
    match serde_json::from_str(text) {
        Ok(value) => Ok(value),
        Err(e) if text.starts_with(['{', '[', '"']) => Err(format!("invalid JSON: {e}")),
        Err(_) => Ok(Value::String(text.to_owned())),
    }
}

/// Sets `value` at the dotted `path` of `object`, creating the objects on the way.
fn insert_path(object: &mut serde_json::Map<String, Value>, path: &str, value: Value) {
    match path.split_once('.') {
        None => {
            object.insert(path.to_owned(), value);
        }
        Some((head, rest)) => {
            let child = object
                .entry(head.to_owned())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Value::Object(inner) = child {
                insert_path(inner, rest, value);
            }
        }
    }
}

/// `None` when `text` is a `csChargingProfiles` object that fits `SetChargingProfile` (with
/// `connector`) and the typed profile.
fn validate_profile(text: &str, connector: &str) -> Option<String> {
    let value = match json_object(text) {
        Ok(v) => v,
        Err(e) => return Some(e),
    };
    let connector_id: u32 = connector.parse().unwrap_or(0);
    let request = serde_json::json!({"connectorId": connector_id, "csChargingProfiles": value});
    if let Err(e) = schemas().validate_request("SetChargingProfile", &request) {
        return Some(e.to_string());
    }
    serde_json::from_value::<ChargingProfile>(value)
        .err()
        .map(|e| e.to_string())
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

    fn clear(form: &mut Form) {
        form.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ));
    }

    #[test]
    fn remote_start_requires_a_short_id_tag_and_a_valid_connector() {
        let mut f = Form::remote_start("CP-1");
        assert!(f.submit().is_none());
        assert_eq!(f.fields[0].error.as_deref(), Some("idTag is required"));
        type_text(&mut f, &"x".repeat(21));
        assert!(f.fields[0].error.as_ref().is_some_and(|e| e.contains("21")));
        clear(&mut f);
        type_text(&mut f, "ABC");
        f.handle_key(key(KeyCode::Tab));
        type_text(&mut f, "zero");
        assert!(f.fields[1].error.is_some());
        assert!(f.submit().is_none());
        assert_eq!(f.focus, 1);
        clear(&mut f);
        type_text(&mut f, "2");
        assert_eq!(
            f.submit(),
            Some(Submitted::Send(Command::RemoteStart {
                id_tag: "ABC".into(),
                connector: Some(2)
            }))
        );
        clear(&mut f);
        assert_eq!(
            f.submit(),
            Some(Submitted::Send(Command::RemoteStart {
                id_tag: "ABC".into(),
                connector: None
            })),
            "the connector is optional"
        );
    }

    #[test]
    fn remote_stop_maps_the_option_to_a_transaction_id() {
        let choices = [
            StopChoice {
                id: 7,
                connector: 1,
                id_tag: "A".into(),
            },
            StopChoice {
                id: 9,
                connector: 2,
                id_tag: "B".into(),
            },
        ];
        let mut f = Form::remote_stop("CP-1", &choices);
        assert_eq!(f.fields[0].options.len(), 2);
        assert_eq!(
            f.submit(),
            Some(Submitted::Send(Command::RemoteStop { transaction_id: 7 })),
            "the first one is preselected"
        );
        f.handle_key(key(KeyCode::Right));
        assert_eq!(
            f.submit(),
            Some(Submitted::Send(Command::RemoteStop { transaction_id: 9 }))
        );
    }

    #[test]
    fn reset_trigger_and_availability_pick_from_lists() {
        let mut f = Form::reset("CP-1");
        f.handle_key(key(KeyCode::Char('h')));
        assert_eq!(
            f.submit(),
            Some(Submitted::Send(Command::Reset(ResetType::Hard)))
        );
        let mut f = Form::trigger("CP-1");
        f.handle_key(key(KeyCode::Char('h')));
        assert_eq!(f.fields[0].chosen(), Some("Heartbeat"));
        assert!(matches!(
            f.submit(),
            Some(Submitted::Send(Command::TriggerMessage {
                message: MessageTrigger::Heartbeat,
                connector: None
            }))
        ));
        let mut f = Form::availability("CP-1");
        f.handle_key(key(KeyCode::Tab));
        f.handle_key(key(KeyCode::Char('i')));
        assert!(matches!(
            f.submit(),
            Some(Submitted::Send(Command::ChangeAvailability {
                connector: 0,
                kind: AvailabilityType::Inoperative
            }))
        ));
    }

    #[test]
    fn change_configuration_needs_a_key_and_checks_the_schema() {
        let mut f = Form::change_configuration("CP-1");
        assert!(f.submit().is_none());
        assert_eq!(f.fields[0].error.as_deref(), Some("required"));
        type_text(&mut f, &"k".repeat(51));
        assert!(f.submit().is_none(), "the schema caps the key at 50");
        assert!(f.error.is_some(), "{:?}", f.error);
        clear(&mut f);
        type_text(&mut f, "HeartbeatInterval");
        f.handle_key(key(KeyCode::Tab));
        type_text(&mut f, "60");
        assert_eq!(
            f.submit(),
            Some(Submitted::Send(Command::ChangeConfiguration {
                key: "HeartbeatInterval".into(),
                value: "60".into()
            }))
        );
    }

    #[test]
    fn get_configuration_splits_keys_and_limits_their_length() {
        let mut f = Form::get_configuration("CP-1");
        assert_eq!(
            f.submit(),
            Some(Submitted::Send(Command::GetConfiguration { keys: vec![] }))
        );
        type_text(&mut f, "A, B  C");
        assert_eq!(
            f.submit(),
            Some(Submitted::Send(Command::GetConfiguration {
                keys: vec!["A".into(), "B".into(), "C".into()]
            }))
        );
        type_text(&mut f, &format!(",{}", "z".repeat(51)));
        assert!(f.fields[0].error.is_some());
        assert!(f.submit().is_none());
    }

    #[test]
    fn unlock_and_data_transfer_validate_their_fields() {
        let mut f = Form::unlock("CP-1", Some(2));
        assert!(matches!(
            f.submit(),
            Some(Submitted::Send(Command::UnlockConnector { connector: 2 }))
        ));
        clear(&mut f);
        type_text(&mut f, "0");
        assert!(f.submit().is_none(), "connector 0 cannot be unlocked");
        let mut f = Form::data_transfer("CP-1");
        assert!(f.submit().is_none());
        type_text(&mut f, "Acme");
        assert!(matches!(
            f.submit(),
            Some(Submitted::Send(Command::DataTransfer {
                message_id: None,
                data: None,
                ..
            }))
        ));
    }

    #[test]
    fn profile_json_is_validated_inline_against_the_schema() {
        let mut f = Form::set_profile("CP-1");
        assert_eq!(f.fields[1].error, None, "the template is valid");
        f.focus = 1;
        f.handle_key(key(KeyCode::End));
        f.handle_key(key(KeyCode::Backspace));
        assert!(
            f.fields[1]
                .error
                .as_ref()
                .is_some_and(|e| e.contains("invalid JSON"))
        );
        assert!(f.submit().is_none());
        assert_eq!(f.focus, 1);
        f.handle_key(key(KeyCode::Char('}')));
        assert_eq!(f.fields[1].error, None);
        // an unknown purpose violates the schema
        f.fields[1].input =
            TextInput::new(PROFILE_TEMPLATE.replace("ChargePointMaxProfile", "Bogus"));
        f.handle_key(key(KeyCode::End));
        assert!(f.fields[1].error.is_some());
        f.fields[1].input = TextInput::new(PROFILE_TEMPLATE);
        f.handle_key(key(KeyCode::End));
        assert!(matches!(
            f.submit(),
            Some(Submitted::Send(Command::SetChargingProfile {
                connector: 0,
                ..
            }))
        ));
    }

    #[test]
    fn raw_payload_is_checked_against_the_chosen_action() {
        let mut f = Form::raw("CP-1", Some("Reset"));
        assert_eq!(f.fields[0].options.len(), 26);
        assert_eq!(
            f.fields[1].input.text(),
            r#"{"type":"Hard"}"#,
            "the payload starts as the minimal valid one"
        );
        assert_eq!(f.fields[1].error, None);
        f.focus = 1;
        clear(&mut f);
        assert!(f.submit().is_none(), "{{}} lacks the type");
        assert!(f.fields[1].error.is_some());
        type_text(&mut f, r#"{"type":"Soft"}"#);
        assert_eq!(f.fields[1].error, None);
        assert!(matches!(
            f.submit(),
            Some(Submitted::Send(Command::Raw { ref action, .. })) if action == "Reset"
        ));
        // switching to another action re-validates the same payload
        f.focus = 0;
        f.handle_key(key(KeyCode::Char('c'))); // CancelReservation
        assert_ne!(f.fields[0].chosen(), Some("Reset"));
        assert!(f.fields[1].error.is_some());
        f.focus = 1;
        clear(&mut f);
        type_text(&mut f, "[1]");
        assert_eq!(f.fields[1].error.as_deref(), Some("must be a JSON object"));
    }

    #[test]
    fn rule_form_needs_a_json_object() {
        let mut f = Form::rule();
        assert!(matches!(f.submit(), Some(Submitted::Rule { .. })));
        f.focus = 1;
        f.handle_key(key(KeyCode::End));
        f.handle_key(key(KeyCode::Backspace));
        assert!(f.fields[1].error.is_some());
        assert!(f.submit().is_none());
    }

    #[test]
    fn fault_payload_matters_only_for_invalid_payload() {
        let mut f = Form::fault();
        f.focus = 3;
        clear(&mut f);
        type_text(&mut f, "nope");
        assert_eq!(f.fields[3].error, None, "ignored for call_error");
        assert!(matches!(
            f.submit(),
            Some(Submitted::Fault {
                kind: FaultKind::CallError,
                times: 1,
                ..
            })
        ));
        f.focus = 1;
        f.handle_key(key(KeyCode::Char('i')));
        assert_eq!(f.fields[1].chosen(), Some("invalid_payload"));
        assert!(f.fields[3].error.is_some(), "re-validated with the kind");
        assert!(f.submit().is_none());
        f.focus = 2;
        clear(&mut f);
        type_text(&mut f, "0");
        assert!(f.fields[2].error.is_some());
        clear(&mut f);
        type_text(&mut f, "3");
        f.focus = 3;
        clear(&mut f);
        type_text(&mut f, r#"{"x":1}"#);
        let Some(Submitted::Fault {
            kind: FaultKind::InvalidPayload,
            times: 3,
            payload,
            ..
        }) = f.submit()
        else {
            panic!("expected an invalid_payload fault");
        };
        assert_eq!(payload, serde_json::json!({"x": 1}));
    }

    #[test]
    fn remove_rule_picks_from_active_rules_and_escape_cancels() {
        let mut f = Form::remove_rule(&["Authorize".into(), "Heartbeat".into()]);
        f.handle_key(key(KeyCode::Right));
        assert_eq!(f.submit(), Some(Submitted::RemoveRule("Heartbeat".into())));
        assert_eq!(f.handle_key(key(KeyCode::Esc)), FormEvent::Cancel);
        assert_eq!(f.handle_key(key(KeyCode::Enter)), FormEvent::Submit);
    }

    #[test]
    fn paste_is_validated_like_typing() {
        let mut f = Form::remote_start("CP-1");
        f.paste(&"é".repeat(20));
        assert_eq!(f.fields[0].error, None);
        f.paste("x");
        assert!(f.fields[0].error.is_some());
    }

    #[test]
    fn data_transfer_limits_apply_inline() {
        let mut f = Form::data_transfer("CP-1");
        type_text(&mut f, &"v".repeat(256));
        assert!(f.fields[0].error.as_ref().unwrap().contains("at most 255"));
        assert!(f.submit().is_none());
        clear(&mut f);
        type_text(&mut f, "Acme");
        f.handle_key(key(KeyCode::Tab));
        f.paste(&"m".repeat(51));
        assert!(f.fields[1].error.as_ref().unwrap().contains("at most 50"));
        clear(&mut f);
        f.handle_key(key(KeyCode::Tab));
        f.paste(&"d".repeat(64 * 1024 + 1));
        assert!(
            f.fields[2]
                .error
                .as_ref()
                .unwrap()
                .contains("at most 65536")
        );
        clear(&mut f);
        f.paste(&"d".repeat(64 * 1024));
        assert!(f.submit().is_some());
    }

    #[test]
    fn free_text_values_keep_their_spaces() {
        let mut f = Form::data_transfer("CP-1");
        type_text(&mut f, " Acme ");
        f.handle_key(key(KeyCode::Tab));
        f.handle_key(key(KeyCode::Tab));
        f.paste("  a b  ");
        let Some(Submitted::Send(Command::DataTransfer {
            vendor_id, data, ..
        })) = f.submit()
        else {
            panic!("valid");
        };
        assert_eq!(vendor_id, "Acme", "ids are trimmed");
        assert_eq!(data.as_deref(), Some("  a b  "));
        let mut f = Form::change_configuration("CP-1");
        type_text(&mut f, " Key ");
        f.handle_key(key(KeyCode::Tab));
        f.paste(" v ");
        assert_eq!(
            f.submit(),
            Some(Submitted::Send(Command::ChangeConfiguration {
                key: "Key".into(),
                value: " v ".into()
            }))
        );
    }

    #[test]
    fn input_is_capped_with_a_visible_notice() {
        let mut f = Form::data_transfer("CP-1");
        f.handle_key(key(KeyCode::Tab));
        f.handle_key(key(KeyCode::Tab));
        f.paste(&"x".repeat(INPUT_CAP + 1000));
        assert_eq!(f.fields[2].input.text().len(), INPUT_CAP);
        assert!(f.fields[2].capped);
        assert!(f.fields[2].error.is_some());
        type_text(&mut f, "y");
        assert_eq!(f.fields[2].input.text().len(), INPUT_CAP);
        f.handle_key(key(KeyCode::Backspace));
        assert!(!f.fields[2].capped);
        // Free text without its own limit shows the cap notice itself.
        let mut f = Form::change_configuration("CP-1");
        f.handle_key(key(KeyCode::Tab));
        f.paste(&"x".repeat(INPUT_CAP + 1));
        assert_eq!(f.fields[1].error.as_deref(), Some(CAP_NOTICE));
    }
}
