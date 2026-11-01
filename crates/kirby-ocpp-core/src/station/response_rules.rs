//! Response rules: make the station misbehave on purpose when the CSMS sends a command.
//!
//! A rule overrides the normal handling of one inbound action (one of the 19 CSMS commands).
//! It exists to test how a CSMS reacts to a flaky or hostile charge point.
//!
//! Compact text form (CLI `--rule`, TUI form): `<Action>=<behavior>[*<count>]` with
//!
//! | behavior                       | effect                                                        |
//! |--------------------------------|---------------------------------------------------------------|
//! | `reject`                       | the action's own rejection status, else CALLERROR NotSupported |
//! | `delay:<ms>`                   | handled normally, the reply is sent `ms` later                |
//! | `error:<Code>[:<description>]` | CALLERROR with an OCPP-J error code                           |
//! | `drop`                         | close the WebSocket without replying                          |
//! | `ignore`                       | never reply                                                   |
//!
//! `*N` applies the rule `N` times and then removes it (default: unlimited). There is at most one
//! rule per action; setting another replaces it. Only `delay` runs the real handler: the other
//! behaviors do **not** execute the command (no side effects).
//!
//! This module is pure (parsing, formatting, the rule list, the rejection payloads).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::ocpp::action::Action;
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::messages::{
    CancelReservation, ChangeAvailability, ChangeConfiguration, ClearCache, ClearChargingProfile,
    DataTransfer, GetCompositeSchedule, GetConfiguration, GetDiagnostics, GetLocalListVersion,
    RemoteStartTransaction, RemoteStopTransaction, ReserveNow, Reset, SendLocalList,
    SetChargingProfile, TriggerMessage, UnlockConnector, UpdateFirmware,
};

/// The inbound actions a rule can target.
pub const INBOUND_ACTIONS: [&str; 19] = [
    CancelReservation::NAME,
    ChangeAvailability::NAME,
    ChangeConfiguration::NAME,
    ClearCache::NAME,
    ClearChargingProfile::NAME,
    DataTransfer::NAME,
    GetCompositeSchedule::NAME,
    GetConfiguration::NAME,
    GetDiagnostics::NAME,
    GetLocalListVersion::NAME,
    RemoteStartTransaction::NAME,
    RemoteStopTransaction::NAME,
    ReserveNow::NAME,
    Reset::NAME,
    SendLocalList::NAME,
    SetChargingProfile::NAME,
    TriggerMessage::NAME,
    UnlockConnector::NAME,
    UpdateFirmware::NAME,
];

/// Longest accepted `delay` (one hour).
pub const MAX_DELAY_MS: u64 = 3_600_000;

/// What to do instead of the normal handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Behavior {
    /// The action's natural rejection (see [`reject_payload`]), else CALLERROR `NotSupported`.
    Reject,
    /// Handle normally; send the reply after `ms` milliseconds.
    Delay { ms: u64 },
    /// Reply with a CALLERROR.
    Error {
        code: ErrorCode,
        description: Option<String>,
    },
    /// Close the connection without replying.
    Drop,
    /// Never reply (the CSMS call timeout fires).
    Ignore,
}

/// One rule: `behavior` for the next `remaining` CALLs of `action`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseRule {
    /// Canonical action name, one of [`INBOUND_ACTIONS`].
    pub action: String,
    pub behavior: Behavior,
    /// Applications left; `None` = unlimited.
    pub remaining: Option<u32>,
}

/// A rule that cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuleError {
    #[error(
        "expected <Action>=<behavior>[*<count>], e.g. Reset=reject or GetConfiguration=delay:5000"
    )]
    Format,
    #[error("unknown action `{0}` (expected one of the 19 CSMS commands, e.g. Reset)")]
    UnknownAction(String),
    #[error(
        "unknown behavior `{0}` (expected reject, delay:<ms>, error:<Code>[:<text>], drop or ignore)"
    )]
    UnknownBehavior(String),
    #[error("delay must be a number of milliseconds between 0 and {MAX_DELAY_MS}")]
    BadDelay,
    #[error("unknown error code `{0}` (expected one of the ten OCPP 1.6 CALLERROR codes)")]
    UnknownErrorCode(String),
    #[error("count must be a positive integer")]
    BadCount,
}

/// Canonical spelling of an inbound action name (case-insensitive match).
pub fn canonical_action(name: &str) -> Result<&'static str, RuleError> {
    INBOUND_ACTIONS
        .iter()
        .find(|known| known.eq_ignore_ascii_case(name.trim()))
        .copied()
        .ok_or_else(|| RuleError::UnknownAction(name.trim().to_owned()))
}

impl FromStr for ResponseRule {
    type Err = RuleError;

    fn from_str(text: &str) -> Result<Self, RuleError> {
        let (action, rest) = text.split_once('=').ok_or(RuleError::Format)?;
        let action = canonical_action(action)?.to_owned();
        let rest = rest.trim();
        // A trailing `*<digits>` is the count; anything else stays part of the behavior
        // (an error description may contain a star).
        let (behavior_text, remaining) = match rest.rsplit_once('*') {
            Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                let count: u32 = tail.parse().map_err(|_| RuleError::BadCount)?;
                if count == 0 {
                    return Err(RuleError::BadCount);
                }
                (head.trim_end(), Some(count))
            }
            _ => (rest, None),
        };
        let behavior = parse_behavior(behavior_text)?;
        Ok(Self {
            action,
            behavior,
            remaining,
        })
    }
}

fn parse_behavior(text: &str) -> Result<Behavior, RuleError> {
    let (name, arg) = match text.split_once(':') {
        Some((name, arg)) => (name.trim(), Some(arg)),
        None => (text.trim(), None),
    };
    match (name.to_ascii_lowercase().as_str(), arg) {
        ("reject", None) => Ok(Behavior::Reject),
        ("drop", None) => Ok(Behavior::Drop),
        ("ignore", None) => Ok(Behavior::Ignore),
        ("delay", Some(ms)) => {
            let ms: u64 = ms.trim().parse().map_err(|_| RuleError::BadDelay)?;
            if ms > MAX_DELAY_MS {
                return Err(RuleError::BadDelay);
            }
            Ok(Behavior::Delay { ms })
        }
        ("delay", None) => Err(RuleError::BadDelay),
        ("error", Some(arg)) => {
            let (code, description) = match arg.split_once(':') {
                Some((code, text)) => (code.trim(), Some(text.trim())),
                None => (arg.trim(), None),
            };
            let code = ErrorCode::STANDARD
                .iter()
                .find(|known| known.as_str().eq_ignore_ascii_case(code))
                .cloned()
                .ok_or_else(|| RuleError::UnknownErrorCode(code.to_owned()))?;
            Ok(Behavior::Error {
                code,
                description: description.filter(|t| !t.is_empty()).map(str::to_owned),
            })
        }
        ("error", None) => Err(RuleError::UnknownErrorCode(String::new())),
        _ => Err(RuleError::UnknownBehavior(text.trim().to_owned())),
    }
}

impl fmt::Display for Behavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reject => f.write_str("reject"),
            Self::Delay { ms } => write!(f, "delay:{ms}"),
            Self::Error { code, description } => {
                write!(f, "error:{code}")?;
                if let Some(text) = description {
                    write!(f, ":{text}")?;
                }
                Ok(())
            }
            Self::Drop => f.write_str("drop"),
            Self::Ignore => f.write_str("ignore"),
        }
    }
}

/// The compact form; parsing it back yields the same rule.
impl fmt::Display for ResponseRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={}", self.action, self.behavior)?;
        if let Some(count) = self.remaining {
            write!(f, "*{count}")?;
        }
        Ok(())
    }
}

/// The rules of a station: at most one per action, in the order they were set.
pub trait RuleList {
    /// Adds `rule`, replacing the rule of the same action.
    fn set_rule(&mut self, rule: ResponseRule);
    /// Removes the rules of `action`, or all of them for `None`; returns how many went away.
    fn clear_rules(&mut self, action: Option<&str>) -> usize;
    /// Picks the behavior for the next CALL of `action` and uses up one application.
    fn take_rule(&mut self, action: &str) -> Option<Behavior>;
}

impl RuleList for Vec<ResponseRule> {
    fn set_rule(&mut self, rule: ResponseRule) {
        match self.iter_mut().find(|r| r.action == rule.action) {
            Some(slot) => *slot = rule,
            None => self.push(rule),
        }
    }

    fn clear_rules(&mut self, action: Option<&str>) -> usize {
        let before = self.len();
        self.retain(|r| action.is_some_and(|a| r.action != a));
        before - self.len()
    }

    fn take_rule(&mut self, action: &str) -> Option<Behavior> {
        let index = self.iter().position(|r| r.action == action)?;
        let rule = self.get_mut(index)?;
        let behavior = rule.behavior.clone();
        let exhausted = match &mut rule.remaining {
            Some(left) => {
                *left = left.saturating_sub(1);
                *left == 0
            }
            None => false,
        };
        if exhausted {
            self.remove(index);
        }
        Some(behavior)
    }
}

/// The CALLRESULT payload that is the action's own "no": `Rejected`, or `NotSupported` where the
/// protocol has no `Rejected`. `None` for actions whose response has no rejection status
/// (`GetConfiguration`, `GetLocalListVersion`, `ClearChargingProfile`, `GetDiagnostics`,
/// `UpdateFirmware`); the station then answers CALLERROR `NotSupported`.
pub fn reject_payload(action: &str) -> Option<Value> {
    let status = match action {
        a if a == CancelReservation::NAME
            || a == ChangeAvailability::NAME
            || a == ChangeConfiguration::NAME
            || a == ClearCache::NAME
            || a == DataTransfer::NAME
            || a == GetCompositeSchedule::NAME
            || a == RemoteStartTransaction::NAME
            || a == RemoteStopTransaction::NAME
            || a == ReserveNow::NAME
            || a == Reset::NAME
            || a == SetChargingProfile::NAME
            || a == TriggerMessage::NAME =>
        {
            "Rejected"
        }
        a if a == SendLocalList::NAME || a == UnlockConnector::NAME => "NotSupported",
        _ => return None,
    };
    Some(json!({ "status": status }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocpp::schema::SchemaRegistry;

    fn rule(text: &str) -> ResponseRule {
        text.parse().expect("valid rule")
    }

    #[test]
    fn parses_every_behavior() {
        assert_eq!(
            rule("Reset=reject"),
            ResponseRule {
                action: "Reset".into(),
                behavior: Behavior::Reject,
                remaining: None
            }
        );
        assert_eq!(
            rule("getconfiguration=Delay:5000").behavior,
            Behavior::Delay { ms: 5000 }
        );
        assert_eq!(rule("Reset=drop").behavior, Behavior::Drop);
        assert_eq!(rule("Reset=ignore*2").remaining, Some(2));
        assert_eq!(
            rule("ChangeConfiguration=error:InternalError").behavior,
            Behavior::Error {
                code: ErrorCode::InternalError,
                description: None
            }
        );
        assert_eq!(
            rule("Reset=error:genericerror:it is: broken*3"),
            ResponseRule {
                action: "Reset".into(),
                behavior: Behavior::Error {
                    code: ErrorCode::GenericError,
                    description: Some("it is: broken".into())
                },
                remaining: Some(3)
            }
        );
    }

    #[test]
    fn display_roundtrips() {
        for text in [
            "Reset=reject",
            "Reset=reject*3",
            "GetConfiguration=delay:5000",
            "Reset=drop*1",
            "Reset=ignore",
            "Reset=error:NotImplemented",
            "ReserveNow=error:SecurityError:no way*2",
        ] {
            assert_eq!(rule(text).to_string(), text);
        }
    }

    #[test]
    fn rejects_malformed_rules() {
        let parse = |t: &str| t.parse::<ResponseRule>().unwrap_err();
        assert_eq!(parse("Reset"), RuleError::Format);
        assert!(matches!(parse("Nope=reject"), RuleError::UnknownAction(_)));
        // Outbound actions are not rule targets.
        assert!(matches!(
            parse("Heartbeat=reject"),
            RuleError::UnknownAction(_)
        ));
        assert!(matches!(
            parse("Reset=explode"),
            RuleError::UnknownBehavior(_)
        ));
        assert!(matches!(
            parse("Reset=reject:1"),
            RuleError::UnknownBehavior(_)
        ));
        assert_eq!(parse("Reset=delay"), RuleError::BadDelay);
        assert_eq!(parse("Reset=delay:abc"), RuleError::BadDelay);
        assert_eq!(parse("Reset=delay:-1"), RuleError::BadDelay);
        assert_eq!(parse("Reset=delay:3600001"), RuleError::BadDelay);
        assert!(matches!(
            parse("Reset=error:Oops"),
            RuleError::UnknownErrorCode(_)
        ));
        assert!(matches!(
            parse("Reset=error"),
            RuleError::UnknownErrorCode(_)
        ));
        assert_eq!(parse("Reset=reject*0"), RuleError::BadCount);
        assert_eq!(parse("Reset=reject*99999999999"), RuleError::BadCount);
    }

    #[test]
    fn set_replaces_and_clear_removes() {
        let mut rules: Vec<ResponseRule> = Vec::new();
        rules.set_rule(rule("Reset=reject"));
        rules.set_rule(rule("ClearCache=drop"));
        rules.set_rule(rule("Reset=ignore"));
        assert_eq!(
            rules.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["Reset=ignore", "ClearCache=drop"]
        );
        assert_eq!(rules.clear_rules(Some("Reset")), 1);
        assert_eq!(rules.clear_rules(Some("Reset")), 0);
        assert_eq!(rules.clear_rules(None), 1);
        assert_eq!(rules.len(), 0);
    }

    #[test]
    fn take_counts_down_then_disappears() {
        let mut rules = vec![rule("Reset=reject*2"), rule("ClearCache=drop")];
        assert_eq!(rules.take_rule("GetConfiguration"), None);
        assert_eq!(rules.take_rule("Reset"), Some(Behavior::Reject));
        assert_eq!(rules[0].remaining, Some(1));
        assert_eq!(rules.take_rule("Reset"), Some(Behavior::Reject));
        assert_eq!(rules.take_rule("Reset"), None);
        // Unlimited rules stay.
        assert_eq!(rules.take_rule("ClearCache"), Some(Behavior::Drop));
        assert_eq!(rules.take_rule("ClearCache"), Some(Behavior::Drop));
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn reject_payloads_are_valid_responses() {
        let schemas = SchemaRegistry::new().expect("schemas");
        let mut without = Vec::new();
        for action in INBOUND_ACTIONS {
            match reject_payload(action) {
                Some(payload) => schemas
                    .validate_response(action, &payload)
                    .unwrap_or_else(|e| panic!("{action}: {e}")),
                None => without.push(action),
            }
        }
        assert_eq!(
            without,
            [
                "ClearChargingProfile",
                "GetConfiguration",
                "GetDiagnostics",
                "GetLocalListVersion",
                "UpdateFirmware"
            ]
        );
    }

    #[test]
    fn inbound_action_names_are_unique_and_known() {
        let schemas = SchemaRegistry::new().expect("schemas");
        let mut seen = std::collections::HashSet::new();
        for action in INBOUND_ACTIONS {
            assert!(seen.insert(action), "{action} twice");
            assert!(schemas.knows_action(action));
        }
    }
}
