//! Per-protocol action registry: the actions a central system can send and the actions a
//! station can send, per OCPP version, with the metadata the terminal UI, the stdin and
//! script commands and the HTTP API share.
//!
//! The types here are version-neutral. Each protocol contributes one static table, found by
//! its WebSocket subprotocol name through [`protocol_actions`]. `ocpp1.6` (see [`ocpp16`])
//! and `ocpp2.0.1` (see [`ocpp201`]) have one; a protocol without a table has no registered
//! actions.
//!
//! ```
//! use kirby_csms_core::registry;
//!
//! let reset = registry::csms_action("ocpp1.6", "Reset").expect("registered");
//! assert_eq!(reset.title, "Reset");
//! assert!(registry::is_station_action("ocpp1.6", "Heartbeat"));
//! assert!(registry::is_station_action("ocpp2.0.1", "TransactionEvent"));
//! assert!(registry::protocol_actions("ocpp9.9").is_none());
//! ```

pub mod ocpp16;
pub mod ocpp201;

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
const PROTOCOLS: &[&ProtocolActions] = &[&ocpp16::ACTIONS, &ocpp201::ACTIONS];

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
