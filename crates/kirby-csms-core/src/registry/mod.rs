//! Per-protocol action registry: the actions a central system can send and the actions a
//! station can send, per OCPP version, with the metadata the terminal UI, the stdin and
//! script commands and the HTTP API share.
//!
//! The types here are version-neutral. Each protocol contributes one static table, found by
//! its WebSocket subprotocol name through [`protocol_actions`]. `ocpp1.6` (see [`ocpp16`]),
//! `ocpp2.0.1` (see [`ocpp201`]) and `ocpp2.1` (see [`ocpp21`]) have one; a protocol without a
//! table has no registered actions.
//!
//! ```
//! use kirby_csms_core::registry;
//!
//! let reset = registry::csms_action("ocpp1.6", "Reset").expect("registered");
//! assert_eq!(reset.title, "Reset");
//! assert!(registry::is_station_action("ocpp1.6", "Heartbeat"));
//! assert!(registry::is_station_action("ocpp2.0.1", "TransactionEvent"));
//! assert!(registry::is_station_action("ocpp2.1", "NotifySettlement"));
//! assert!(registry::protocol_actions("ocpp9.9").is_none());
//! ```

use kirby_ocpp_core::ocpp::v2::registry as v2_registry;
use kirby_ocpp_core::ocpp::{PayloadKind, Protocol, SchemaRegistry};
use serde_json::Value;

pub mod ocpp16;
pub mod ocpp201;
pub mod ocpp21;
mod template;

pub use template::payload_template;

/// Where an action comes from, for grouping in menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ActionGroup {
    /// Part of the base specification of the protocol.
    Core,
    /// An extension of the security whitepaper (OCPP 1.6 only; 2.0.1 includes security in
    /// the base specification).
    Security,
}

impl ActionGroup {
    /// A stable lowercase name: `core` or `security`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Security => "security",
        }
    }
}

/// How a form field is edited and checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldType {
    /// Free text.
    Text,
    /// A whole number (optionally with a lower bound given in the hint).
    Integer,
    /// One of the listed choices; an empty list means the choices are known only at run
    /// time (for example the active transactions of a station).
    Choice,
    /// A JSON object.
    Json,
    /// One or more PEM blocks.
    Pem,
    /// An RFC 3339 time.
    Timestamp,
    /// `true` or `false`, sent as a JSON boolean; the default, if any, is `"true"` or
    /// `"false"`.
    Boolean,
}

/// One input of an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpec {
    /// The JSON key in the request payload; a dotted path (`log.remoteLocation`) is a key of
    /// a nested object.
    pub name: &'static str,
    /// The label a form shows.
    pub label: &'static str,
    /// The type of the value.
    pub ty: FieldType,
    /// The allowed values of a [`FieldType::Choice`] field, in menu order.
    pub choices: &'static [&'static str],
    /// The initial value, when there is a fixed one.
    pub default: Option<&'static str>,
    /// Whether the request is invalid without a value.
    pub required: bool,
    /// A short note for the person filling the form.
    pub hint: &'static str,
}

/// One action a central system can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionSpec {
    /// The action name as it appears in a CALL.
    pub name: &'static str,
    /// Menu group.
    pub group: ActionGroup,
    /// The title of the palette entry.
    pub title: &'static str,
    /// The one-line description of the palette entry.
    pub summary: &'static str,
    /// The inputs, in form order.
    pub fields: &'static [FieldSpec],
}

impl ActionSpec {
    /// The field whose JSON key is `name`.
    pub fn field(&self, name: &str) -> Option<&'static FieldSpec> {
        self.fields.iter().find(|field| field.name == name)
    }
}

/// The actions of one protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolActions {
    /// The WebSocket subprotocol name (`ocpp1.6`).
    pub protocol: &'static str,
    /// Actions the central system sends, in palette order.
    pub csms_actions: &'static [ActionSpec],
    /// Actions a station sends (the ones rules and faults can target), sorted.
    pub station_actions: &'static [&'static str],
}

/// Every protocol that has a table.
const PROTOCOLS: &[&ProtocolActions] = &[&ocpp16::ACTIONS, &ocpp201::ACTIONS, &ocpp21::ACTIONS];

/// The subprotocol names that have a table.
pub fn protocols() -> impl Iterator<Item = &'static str> {
    PROTOCOLS.iter().map(|table| table.protocol)
}

/// The table of `protocol`, if it has one.
pub fn protocol_actions(protocol: &str) -> Option<&'static ProtocolActions> {
    PROTOCOLS
        .iter()
        .find(|table| table.protocol == protocol)
        .copied()
}

/// The central-system action `name` of `protocol`.
pub fn csms_action(protocol: &str, name: &str) -> Option<&'static ActionSpec> {
    protocol_actions(protocol)?
        .csms_actions
        .iter()
        .find(|spec| spec.name == name)
}

/// Whether a station of `protocol` can send `action`.
pub fn is_station_action(protocol: &str, action: &str) -> bool {
    protocol_actions(protocol).is_some_and(|table| table.station_actions.contains(&action))
}

/// How a client supplies the request payload of a central-system action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ActionInput {
    /// The action has form fields: the payload is built from them.
    Form,
    /// The action has no form fields but its request has members: the payload is edited as
    /// raw JSON (start from [`payload_template`]).
    Raw,
    /// The action takes no input: the payload is `{}`.
    None,
}

/// How the payload of `spec` (an action of `protocol`) is supplied.
///
/// An action without form fields takes no input when its payload layer types it (a form
/// elsewhere builds the request) or when its request schema has no member besides
/// `customData`. A protocol or action the schemas do not know counts as taking raw input.
pub fn takes_input(protocol: &str, spec: &ActionSpec) -> ActionInput {
    if !spec.fields.is_empty() {
        return ActionInput::Form;
    }
    let protocol_id = Protocol::from_token(protocol);
    let typed = protocol_id
        .is_some_and(|id| v2_registry::lookup_for(id, spec.name).is_some_and(|info| info.typed));
    if typed || !has_request_input(protocol_id, spec.name) {
        ActionInput::None
    } else {
        ActionInput::Raw
    }
}

/// Whether the request schema of `action` has a member other than `customData`; `true` when
/// the schema is unknown.
fn has_request_input(protocol: Option<Protocol>, action: &str) -> bool {
    let Some(schemas) = protocol.and_then(|id| SchemaRegistry::for_protocol(id).ok()) else {
        return true;
    };
    let Some(source) = schemas.schema_source(action, PayloadKind::Request) else {
        return true;
    };
    serde_json::from_str::<Value>(source).map_or(true, |schema| {
        schema
            .get("properties")
            .and_then(Value::as_object)
            .is_none_or(|members| members.keys().any(|key| key != "customData"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(protocol: &str, name: &str) -> ActionInput {
        let spec = csms_action(protocol, name).expect("registered action");
        takes_input(protocol, spec)
    }

    #[test]
    fn actions_with_fields_are_forms() {
        assert_eq!(input("ocpp1.6", "Reset"), ActionInput::Form);
        assert_eq!(input("ocpp2.1", "UsePriorityCharging"), ActionInput::Form);
    }

    #[test]
    fn raw_and_no_input_actions() {
        assert_eq!(input("ocpp2.1", "SetDERControl"), ActionInput::Raw);
        assert_eq!(
            input("ocpp2.1", "GetPeriodicEventStream"),
            ActionInput::None
        );
        assert_eq!(input("ocpp1.6", "ClearCache"), ActionInput::None);
    }

    #[test]
    fn unknown_protocol_or_action_is_raw() {
        let spec = ActionSpec {
            name: "NoSuchAction",
            group: ActionGroup::Core,
            title: "",
            summary: "",
            fields: &[],
        };
        assert_eq!(takes_input("ocpp1.6", &spec), ActionInput::Raw);
        assert_eq!(takes_input("ocpp9.9", &spec), ActionInput::Raw);
    }

    #[test]
    fn typed_action_without_fields_takes_no_input() {
        let spec = ActionSpec {
            name: "GetBaseReport",
            group: ActionGroup::Core,
            title: "",
            summary: "",
            fields: &[],
        };
        assert_eq!(takes_input("ocpp2.0.1", &spec), ActionInput::None);
    }
}
