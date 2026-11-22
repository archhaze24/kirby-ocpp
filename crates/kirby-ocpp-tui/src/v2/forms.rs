//! Builders of the OCPP 2.x forms and pick-lists on top of the shared engine
//! ([`Form::generic`], [`Choice::generic`]) and the conversion of a submitted form into
//! commands.

use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::SchemaRegistry;
use kirby_ocpp_core::ocpp::v2::registry::actions_for;
use kirby_ocpp_core::ocpp::v2::types::{
    ChargingState, Component, IdTokenType, StopReason, Variable, WireEnum,
};
use kirby_ocpp_core::station2::{Command2, TransactionLimit2, VariableView};
use serde_json::{Value, json};

use super::app::{App2, Effect2, Mode2};
use crate::app::{NoticeLevel, event_line};
use crate::forms::{
    Choice, ChoiceKind, Field, FieldKind, FieldValue, Form, ID_TOKEN_MAX_V21, Submitted,
};
use crate::text::sanitize;

/// Names of the generic forms (the `kind` of [`Submitted::Generic`]).
const AUTHORIZE: &str = "authorize";
const PRESENT_TOKEN: &str = "present_token";
const METER_CUSTOM: &str = "meter_custom";
const DATA_TRANSFER: &str = "data_transfer";
const RAW_CALL: &str = "raw_call";
const SECURITY_EVENT: &str = "security_event";
const SET_VARIABLE: &str = "set_variable";
const EXTERNAL_LIMIT: &str = "external_limit";
const CLEAR_EXTERNAL_LIMIT: &str = "clear_external_limit";
const TRANSACTION_LIMIT: &str = "transaction_limit";

/// Names of the generic pick-lists (the name of a [`ChoiceKind::Generic`]).
const STOP_REASON: &str = "stop_reason";
const CHARGING_STATE: &str = "charging_state";

/// Most characters of a security event type, of a device-model name and instance.
const EVENT_TYPE_MAX: usize = 50;
const NAME_MAX: usize = 50;
/// Most characters of `techInfo`.
const TECH_INFO_MAX: usize = 255;
/// Most characters of a `DataTransfer` vendor id and message id.
const VENDOR_ID_MAX: usize = 255;
const MESSAGE_ID_MAX: usize = 50;
/// Most characters of a variable value (`attributeValue`).
const VARIABLE_VALUE_MAX: usize = 2500;

/// The security events of OCPP 2.0.1 Part 2 Appendix 1 ("Security Events"), in the order of
/// the table.
pub const SECURITY_EVENT_NAMES: [&str; 21] = [
    "FirmwareUpdated",
    "FailedToAuthenticateAtCsms",
    "CsmsFailedToAuthenticate",
    "SettingSystemTime",
    "StartupOfTheDevice",
    "ResetOrReboot",
    "SecurityLogWasCleared",
    "ReconfigurationOfSecurityParameters",
    "MemoryExhaustion",
    "InvalidMessages",
    "AttemptedReplayAttacks",
    "TamperDetectionActivated",
    "InvalidFirmwareSignature",
    "InvalidFirmwareSigningCertificate",
    "InvalidCsmsCertificate",
    "InvalidChargingStationCertificate",
    "DiscardedRenewedClientCertificate",
    "InvalidTLSVersion",
    "InvalidTLSCipherSuite",
    "MaintenanceLoginAccepted",
    "MaintenanceLoginFailed",
];

fn names<T: WireEnum>() -> Vec<String> {
    T::ALL.iter().map(|v| v.as_str().to_owned()).collect()
}

/// The `IdTokenEnumType` select, on `Local` (the usual type of a typed-in token).
fn token_type_field() -> Field {
    let options = IdTokenType::KNOWN
        .iter()
        .map(|t| t.as_str().to_owned())
        .collect();
    let mut field = Field::new("type", FieldKind::Select, "").with_options(options);
    if let Some(i) = IdTokenType::KNOWN
        .iter()
        .position(|t| *t == IdTokenType::Local)
    {
        field.option = i;
    }
    field
}

fn token_type(value: &str) -> Option<IdTokenType> {
    IdTokenType::KNOWN
        .iter()
        .find(|t| t.as_str() == value)
        .cloned()
}

/// The `idToken` input: 36 characters at most in OCPP 2.0.1, 255 in 2.1.
fn id_token_field(protocol: Protocol) -> Field {
    if protocol == Protocol::V21 {
        Field::new("idToken (at most 255 characters)", FieldKind::IdToken, "")
            .with_max_chars(ID_TOKEN_MAX_V21)
    } else {
        Field::new("idToken (at most 36 characters)", FieldKind::IdToken, "")
    }
}

/// `Authorize`: an `idToken` and its type.
pub(super) fn authorize_form(protocol: Protocol) -> Form {
    Form::generic(
        AUTHORIZE,
        "Authorize",
        vec![id_token_field(protocol), token_type_field()],
        1,
    )
}

/// Present a token: at the selected EVSE, or at the one the station picks (the first with an
/// EV plugged in and no transaction).
pub(super) fn present_token_form(evse: u32, protocol: Protocol) -> Form {
    let mut target = Field::new("EVSE", FieldKind::Select, "").with_options(vec![
        format!("EVSE {evse} (selected)"),
        "any (the station picks one)".to_owned(),
    ]);
    if evse == 0 {
        target.option = 1;
    }
    Form::generic(
        PRESENT_TOKEN,
        "Present token",
        vec![target, id_token_field(protocol), token_type_field()],
        1,
    )
}

/// Advance the meter of an EVSE, with or without a sample.
pub(super) fn meter_custom_form(evse: u32, max_evse: u32) -> Form {
    Form::generic(
        METER_CUSTOM,
        "Meter",
        vec![
            Field::new("EVSE", FieldKind::Evse, &evse.to_string()),
            Field::new("Wh delta", FieldKind::Wh, "120"),
            Field::new("send a sample", FieldKind::Select, "")
                .with_options(vec!["yes".to_owned(), "no".to_owned()]),
        ],
        max_evse,
    )
}

/// A limit of the external system: the EVSE (0 is the whole station), the value and its unit.
pub(super) fn external_limit_form(max_evse: u32) -> Form {
    Form::generic(
        EXTERNAL_LIMIT,
        "External limit",
        vec![
            Field::new("EVSE (0 = whole station)", FieldKind::StationEvse, "0"),
            Field::new("limit", FieldKind::Limit, "16"),
            Field::new("unit", FieldKind::Select, "")
                .with_options(vec!["A".to_owned(), "W".to_owned()]),
        ],
        max_evse,
    )
}

/// A station-side limit of the ongoing transaction (2.1); every limit is optional.
pub(super) fn transaction_limit_form(evse: u32, max_evse: u32) -> Form {
    Form::generic(
        TRANSACTION_LIMIT,
        "Transaction limit",
        vec![
            Field::new("EVSE", FieldKind::Evse, &evse.to_string()),
            Field::new("max energy, kWh (optional)", FieldKind::OptionalText, ""),
            Field::new("max time, s (optional)", FieldKind::OptionalText, ""),
            Field::new("max cost (optional)", FieldKind::OptionalText, ""),
        ],
        max_evse,
    )
}

/// Release the external limit of an EVSE (0 is the whole station).
pub(super) fn clear_external_limit_form(max_evse: u32) -> Form {
    Form::generic(
        CLEAR_EXTERNAL_LIMIT,
        "Clear external limit",
        vec![Field::new(
            "EVSE (0 = whole station)",
            FieldKind::StationEvse,
            "0",
        )],
        max_evse,
    )
}

/// `DataTransfer`; `data` is any JSON value.
pub(super) fn data_transfer_form(vendor: &str) -> Form {
    Form::generic(
        DATA_TRANSFER,
        "DataTransfer",
        vec![
            Field::new("vendorId", FieldKind::Text, vendor).with_max_chars(VENDOR_ID_MAX),
            Field::new("messageId (optional)", FieldKind::OptionalText, "")
                .with_max_chars(MESSAGE_ID_MAX),
            Field::new(
                "data (optional JSON value: \"text\", 42, [1,2], {\"a\":1})",
                FieldKind::JsonValue,
                "",
            ),
        ],
        1,
    )
}

/// Any CALL the station of `protocol` may send, checked against the schema of that version on
/// submit.
pub(super) fn raw_call_form(protocol: Protocol) -> Form {
    let options: Vec<String> = actions_for(protocol)
        .iter()
        .filter(|a| a.direction.station_may_send())
        .map(|a| a.name.to_owned())
        .collect();
    let mut action = Field::new("action", FieldKind::Select, "").with_options(options);
    if let Some(i) = action.options.iter().position(|o| o == "Heartbeat") {
        action.option = i;
    }
    Form::generic(
        RAW_CALL,
        "Raw CALL",
        vec![
            action,
            Field::new("payload (JSON object)", FieldKind::JsonValue, "{}"),
        ],
        1,
    )
}

/// `SecurityEventNotification`: a type from Appendix 1 or a custom one, and `techInfo`.
pub(super) fn security_event_form() -> Form {
    Form::generic(
        SECURITY_EVENT,
        "Security event",
        vec![
            Field::new("event type", FieldKind::Select, "").with_options(
                SECURITY_EVENT_NAMES
                    .iter()
                    .map(|n| (*n).to_owned())
                    .collect(),
            ),
            Field::new(
                "custom type (optional, replaces the list, 1-50 characters)",
                FieldKind::OptionalText,
                "",
            )
            .with_max_chars(EVENT_TYPE_MAX),
            Field::new(
                "techInfo (optional, at most 255 characters)",
                FieldKind::OptionalText,
                "",
            )
            .with_max_chars(TECH_INFO_MAX),
        ],
        1,
    )
}

/// Whether the value of a variable must not be drawn: write-only variables and the
/// `BasicAuthPassword` of any component.
pub(super) fn is_secret_variable(view: &VariableView) -> bool {
    use kirby_ocpp_core::ocpp::v2::messages::Mutability;
    view.mutability == Mutability::WriteOnly
        || view.variable.eq_ignore_ascii_case("BasicAuthPassword")
}

/// Edit one device-model variable (sets the `Actual` value, like `SetVariables`).
pub(super) fn set_variable_form(view: &VariableView) -> Form {
    let secret = is_secret_variable(view);
    let mut value = Field::new(
        "value",
        FieldKind::OptionalText,
        if secret {
            ""
        } else {
            view.value.as_deref().unwrap_or("")
        },
    )
    .with_max_chars(VARIABLE_VALUE_MAX);
    if secret {
        value = value.hidden();
    }
    Form::generic(
        SET_VARIABLE,
        &format!(
            "Set {}.{}",
            sanitize(&view.component),
            sanitize(&view.variable)
        ),
        vec![
            Field::new("component", FieldKind::Text, &view.component).with_max_chars(NAME_MAX),
            Field::new(
                "variable instance (optional)",
                FieldKind::OptionalText,
                view.instance.as_deref().unwrap_or(""),
            )
            .with_max_chars(NAME_MAX),
            Field::new("variable", FieldKind::Text, &view.variable).with_max_chars(NAME_MAX),
            value,
        ],
        1,
    )
}

/// The stop reasons (`ReasonEnumType`) of `protocol`, `Local` first: the reason of a stop the
/// driver causes.
pub(super) fn stop_reason_choice(evse: u32, protocol: Protocol) -> Choice {
    let mut items = vec![StopReason::Local.as_str().to_owned()];
    items.extend(
        StopReason::ALL
            .iter()
            .filter(|r| **r != StopReason::Local)
            .filter(|r| protocol == Protocol::V21 || !r.is_v21_only())
            .map(|r| r.as_str().to_owned()),
    );
    Choice::generic(STOP_REASON, "Stop reason", items, evse, "EVSE")
}

/// The charging states (`ChargingStateEnumType`).
pub(super) fn charging_state_choice(evse: u32) -> Choice {
    Choice::generic(
        CHARGING_STATE,
        "Charging state",
        names::<ChargingState>(),
        evse,
        "EVSE",
    )
}

/// The commands a pick-list stands for, with the label of the send. `None` for a list this
/// model did not open.
pub(super) fn choice_effect(c: &Choice) -> Option<Effect2> {
    let ChoiceKind::Generic(name) = c.kind else {
        return None;
    };
    let item = c.items.get(c.selected)?;
    let command = match name {
        STOP_REASON => Command2::StopTransaction {
            evse: c.target,
            reason: *StopReason::ALL.iter().find(|r| r.as_str() == item)?,
        },
        CHARGING_STATE => Command2::SetChargingState {
            evse: c.target,
            state: *ChargingState::ALL.iter().find(|s| s.as_str() == item)?,
        },
        _ => return None,
    };
    Some(Effect2::Send {
        label: format!("{}: {item}", command.name().replace('_', " ")),
        commands: vec![command],
        show_response: false,
    })
}

/// What a submitted form sends.
struct Planned {
    label: String,
    commands: Vec<Command2>,
    show_response: bool,
}

impl Planned {
    fn one(label: impl Into<String>, command: Command2, show_response: bool) -> Self {
        Self {
            label: label.into(),
            commands: vec![command],
            show_response,
        }
    }
}

/// A problem with one field of a submitted form: the field index and the text for it.
type FieldProblem = (usize, String);

fn text(values: &[FieldValue], i: usize) -> &str {
    match values.get(i) {
        Some(FieldValue::Text(t)) => t,
        _ => "",
    }
}

fn selected(values: &[FieldValue], i: usize) -> (usize, &str) {
    match values.get(i) {
        Some(FieldValue::Select { index, value }) => (*index, value),
        _ => (0, ""),
    }
}

fn optional(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

fn number<T: std::str::FromStr>(
    values: &[FieldValue],
    i: usize,
    what: &str,
) -> Result<T, FieldProblem> {
    text(values, i)
        .parse()
        .map_err(|_| (i, format!("{what} is not a number")))
}

/// An optional finite number above 0: `None` for an empty field.
fn optional_positive_f64(
    values: &[FieldValue],
    i: usize,
    what: &str,
) -> Result<Option<f64>, FieldProblem> {
    let raw = text(values, i).trim();
    if raw.is_empty() {
        return Ok(None);
    }
    match raw.parse::<f64>() {
        Ok(n) if n.is_finite() && n > 0.0 => Ok(Some(n)),
        _ => Err((i, format!("{what} must be a number above 0"))),
    }
}

/// An optional whole number above 0: `None` for an empty field.
fn optional_positive_i64(
    values: &[FieldValue],
    i: usize,
    what: &str,
) -> Result<Option<i64>, FieldProblem> {
    let raw = text(values, i).trim();
    if raw.is_empty() {
        return Ok(None);
    }
    match raw.parse::<i64>() {
        Ok(n) if n > 0 => Ok(Some(n)),
        _ => Err((i, format!("{what} must be a whole number above 0"))),
    }
}

fn token_command(
    values: &[FieldValue],
    first: usize,
) -> Result<(String, IdTokenType), FieldProblem> {
    let id_token = text(values, first).to_owned();
    let kind = token_type(selected(values, first + 1).1)
        .ok_or_else(|| (first + 1, "unknown token type".to_owned()))?;
    Ok((id_token, kind))
}

/// Turns the values of the generic form `kind` into the commands to send. `evse` is the
/// selected EVSE.
fn plan(
    kind: &str,
    values: &[FieldValue],
    evse: u32,
    protocol: Protocol,
) -> Result<Planned, FieldProblem> {
    match kind {
        AUTHORIZE => {
            let (id_token, token_type) = token_command(values, 0)?;
            Ok(Planned::one(
                "authorize",
                Command2::Authorize {
                    id_token,
                    token_type,
                },
                false,
            ))
        }
        PRESENT_TOKEN => {
            let (id_token, token_type) = token_command(values, 1)?;
            let evse = (selected(values, 0).0 == 0).then_some(evse);
            Ok(Planned::one(
                "present token",
                Command2::PresentToken {
                    evse,
                    id_token,
                    token_type,
                },
                false,
            ))
        }
        METER_CUSTOM => {
            let evse: u32 = number(values, 0, "EVSE")?;
            let wh_delta: u64 = number(values, 1, "Wh delta")?;
            let mut commands = vec![Command2::AdvanceMeter { evse, wh_delta }];
            if selected(values, 2).0 == 0 {
                commands.push(Command2::SampleMeter { evse });
            }
            Ok(Planned {
                label: format!("meter +{wh_delta} Wh"),
                commands,
                show_response: false,
            })
        }
        EXTERNAL_LIMIT => {
            let evse: u32 = number(values, 0, "EVSE")?;
            let limit: f64 = number(values, 1, "limit")?;
            if !limit.is_finite() || limit <= 0.0 {
                return Err((1, "limit must be a number above 0".to_owned()));
            }
            let unit = selected(values, 2).1;
            let schedule = json!({
                "id": 1000 + evse,
                "chargingRateUnit": unit,
                "chargingSchedulePeriod": [{"startPeriod": 0, "limit": limit}],
            });
            Ok(Planned::one(
                "set external limit",
                Command2::SetExternalLimit {
                    evse,
                    limit: schedule,
                },
                false,
            ))
        }
        TRANSACTION_LIMIT => {
            let evse: u32 = number(values, 0, "EVSE")?;
            let max_energy_wh =
                optional_positive_f64(values, 1, "max energy")?.map(|kwh| kwh * 1000.0);
            let max_time_s = optional_positive_i64(values, 2, "max time")?;
            let max_cost = optional_positive_f64(values, 3, "max cost")?;
            let limit = TransactionLimit2 {
                max_energy_wh,
                max_time_s,
                max_cost,
            };
            if limit == TransactionLimit2::default() {
                return Err((1, "enter at least one limit".to_owned()));
            }
            Ok(Planned::one(
                "set transaction limit",
                Command2::SetTransactionLimit { evse, limit },
                false,
            ))
        }
        CLEAR_EXTERNAL_LIMIT => Ok(Planned::one(
            "clear external limit",
            Command2::ClearExternalLimit {
                evse: number(values, 0, "EVSE")?,
            },
            false,
        )),
        DATA_TRANSFER => {
            let data = match text(values, 2) {
                "" => None,
                raw => Some(
                    serde_json::from_str::<Value>(raw)
                        .map_err(|e| (2, format!("not valid JSON: {e}")))?,
                ),
            };
            Ok(Planned::one(
                "data transfer",
                Command2::DataTransfer {
                    vendor_id: text(values, 0).to_owned(),
                    message_id: optional(text(values, 1)),
                    data,
                },
                true,
            ))
        }
        RAW_CALL => {
            let action = selected(values, 0).1.to_owned();
            let payload: Value = serde_json::from_str(text(values, 1))
                .map_err(|e| (1, format!("invalid JSON: {e}")))?;
            if !payload.is_object() {
                return Err((1, "payload must be a JSON object".to_owned()));
            }
            SchemaRegistry::for_protocol(protocol)
                .map_err(|e| (1, format!("schemas are not available: {e}")))?
                .validate_request(&action, &payload)
                .map_err(|e| (1, e.to_string()))?;
            Ok(Planned::one(
                format!("raw {action}"),
                Command2::SendRaw {
                    action,
                    payload,
                    validate: true,
                },
                true,
            ))
        }
        SECURITY_EVENT => {
            let event_type =
                optional(text(values, 1)).unwrap_or_else(|| selected(values, 0).1.to_owned());
            if event_type.is_empty() || event_type.chars().count() > EVENT_TYPE_MAX {
                return Err((1, "event type must be 1 to 50 characters".to_owned()));
            }
            Ok(Planned::one(
                "security event",
                Command2::SendSecurityEvent {
                    event_type,
                    tech_info: optional(text(values, 2)),
                },
                false,
            ))
        }
        SET_VARIABLE => {
            let component = text(values, 0).to_owned();
            let variable = text(values, 2).to_owned();
            // The label never carries the value: it may be a password.
            let label = format!("set {}.{}", sanitize(&component), sanitize(&variable));
            Ok(Planned::one(
                label,
                Command2::SetVariable {
                    component: Component {
                        evse: None,
                        name: component,
                        instance: None,
                        custom_data: None,
                    },
                    variable: Variable {
                        name: variable,
                        instance: optional(text(values, 1)),
                        custom_data: None,
                    },
                    value: text(values, 3).to_owned(),
                },
                false,
            ))
        }
        _ => Err((0, "unknown form".to_owned())),
    }
}

/// Applies a validated form. A value the schema or the station rules refuse keeps the form
/// open with the reason on the field.
pub(super) fn apply_form(app: &mut App2, mut form: Form, done: Submitted, fx: &mut Vec<Effect2>) {
    match done {
        Submitted::Generic { kind, values } => match plan(kind, &values, app.evse, app.protocol) {
            Ok(p) => fx.push(Effect2::Send {
                label: p.label,
                commands: p.commands,
                show_response: p.show_response,
            }),
            Err((i, why)) => {
                if let Some(field) = form.fields.get_mut(i) {
                    field.error = Some(why);
                    form.focus = i;
                }
                app.mode = Mode2::Form(form);
            }
        },
        Submitted::Search(text) => {
            app.log.search = text;
            let n = app.log.visible_count();
            app.notify(NoticeLevel::Info, format!("{n} matching rows"));
        }
        Submitted::Export(path) => {
            let contents: String = app.log.all().map(|e| event_line(e) + "\n").collect();
            fx.push(Effect2::WriteFile { path, contents });
        }
        Submitted::LoadScenario(path) => {
            app.scenario = crate::app::ScenarioView {
                name: path.clone(),
                running: true,
                ..crate::app::ScenarioView::default()
            };
            fx.push(Effect2::RunScenarioFile(path));
        }
        // The other forms belong to the 1.6 model.
        _ => {}
    }
}
