//! Scoped response rules and faults: which station and protocol they apply to, ids, caps.
//!
//! Resolution order for one CALL from a station: station-scoped faults, then global faults
//! (each in injection order, one occurrence consumed), then schema validation, then the
//! station's rule, then a global rule with a protocol, then a global responder, then the
//! default answer.

use std::collections::HashMap;

use serde_json::Value;

use crate::fault::{Fault, Reply};

/// Most faults kept over all scopes by the capped injection methods.
pub const MAX_FAULTS: usize = 1000;
/// Most faults kept for one station by the capped injection methods.
pub const MAX_FAULTS_PER_STATION: usize = 100;
/// Most bytes of action and payload text kept over all faults by the capped methods.
pub const MAX_FAULT_BYTES: usize = 8 * 1024 * 1024;
/// Most scoped rules kept over all stations (a rule is one action of one station, or one
/// action with a protocol and no station).
pub const MAX_SCOPED_RULES: usize = 1000;
/// Most scoped rules kept for one station.
pub const MAX_RULES_PER_STATION: usize = 64;
/// Most bytes of payload text kept over all scoped rules.
pub const MAX_RULE_BYTES: usize = 8 * 1024 * 1024;

/// Whom a fault or rule applies to. The default applies to every station and protocol.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// The station id (exact match); `None` applies to every station.
    pub station: Option<String>,
    /// The negotiated WebSocket subprotocol (for example `ocpp1.6`); `None` applies to every
    /// station, also one that negotiated none.
    pub protocol: Option<String>,
}

impl Scope {
    /// Every station and protocol.
    pub fn global() -> Self {
        Self::default()
    }

    /// One station, any protocol.
    pub fn station(id: impl Into<String>) -> Self {
        Self {
            station: Some(id.into()),
            protocol: None,
        }
    }

    /// Narrows the scope to one protocol.
    pub fn protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocol = Some(protocol.into());
        self
    }

    fn protocol_matches(&self, protocol: Option<&str>) -> bool {
        self.protocol
            .as_deref()
            .is_none_or(|wanted| Some(wanted) == protocol)
    }
}

/// A stored fault as listed by [`CsmsHandle::faults`](crate::CsmsHandle::faults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultInfo {
    /// Opaque id, stable until the fault is used up or removed.
    pub id: String,
    /// Where the fault applies.
    pub scope: Scope,
    /// The action it applies to.
    pub action: String,
    /// `call_error`, `silence`, `close` or `invalid_payload`.
    pub kind: &'static str,
    /// Occurrences left.
    pub remaining: usize,
}

/// A stored scoped rule as listed by the handle.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleInfo {
    /// Where the rule applies.
    pub scope: Scope,
    /// The action it answers.
    pub action: String,
    /// The fixed CALLRESULT payload.
    pub payload: Value,
}

/// The `kind` name of a fault reply.
pub fn reply_kind(reply: &Reply) -> &'static str {
    match reply {
        Reply::Result(_) => "invalid_payload",
        Reply::Error(..) => "call_error",
        Reply::Silence => "silence",
        Reply::Close => "close",
    }
}

fn fault_bytes(fault: &Fault) -> usize {
    let reply = match &fault.reply {
        Reply::Result(value) => value.to_string().len(),
        Reply::Error(_, text) => text.len(),
        _ => 0,
    };
    fault.action.len().saturating_add(reply)
}

struct StoredFault {
    id: String,
    scope: Scope,
    fault: Fault,
}

struct StoredRule {
    protocol: Option<String>,
    payload: Value,
    bytes: usize,
}

type RuleKey = (Option<String>, String);

/// The scoped faults and rules of the engine (behind the engine's state lock).
#[derive(Default)]
pub(crate) struct Scripted {
    faults: Vec<StoredFault>,
    fault_bytes: usize,
    next_fault: u64,
    rules: HashMap<RuleKey, StoredRule>,
    rule_bytes: usize,
}

impl Scripted {
    /// Stores a fault. `capped` enforces the caps; the fault must have occurrences left.
    pub(crate) fn add_fault(
        &mut self,
        fault: Fault,
        scope: Scope,
        capped: bool,
    ) -> Result<String, String> {
        let added = fault_bytes(&fault);
        if capped {
            if self.faults.len() >= MAX_FAULTS {
                return Err(format!("too many faults stored (at most {MAX_FAULTS})"));
            }
            if let Some(station) = &scope.station {
                let held = self
                    .faults
                    .iter()
                    .filter(|stored| stored.scope.station.as_ref() == Some(station))
                    .count();
                if held >= MAX_FAULTS_PER_STATION {
                    return Err(format!(
                        "too many faults stored for one station (at most {MAX_FAULTS_PER_STATION})"
                    ));
                }
            }
            if self.fault_bytes.saturating_add(added) > MAX_FAULT_BYTES {
                return Err(format!(
                    "stored faults would exceed {} MiB",
                    MAX_FAULT_BYTES / (1024 * 1024)
                ));
            }
        }
        self.next_fault = self.next_fault.wrapping_add(1);
        let id = format!("f{}", self.next_fault);
        self.fault_bytes = self.fault_bytes.saturating_add(added);
        self.faults.push(StoredFault {
            id: id.clone(),
            scope,
            fault,
        });
        Ok(id)
    }

    /// Uses one occurrence of the first fault that applies: station faults before global ones.
    pub(crate) fn take_fault(
        &mut self,
        station: &str,
        protocol: Option<&str>,
        action: &str,
    ) -> Option<Reply> {
        let applies = |stored: &StoredFault, scoped: bool| {
            stored.fault.remaining > 0
                && stored.fault.action == action
                && stored.scope.protocol_matches(protocol)
                && match &stored.scope.station {
                    Some(id) => scoped && id == station,
                    None => !scoped,
                }
        };
        let at = self
            .faults
            .iter()
            .position(|stored| applies(stored, true))
            .or_else(|| self.faults.iter().position(|stored| applies(stored, false)))?;
        let stored = self.faults.get_mut(at)?;
        stored.fault.remaining -= 1;
        let reply = stored.fault.reply.clone();
        if stored.fault.remaining == 0 {
            let freed = fault_bytes(&stored.fault);
            self.fault_bytes = self.fault_bytes.saturating_sub(freed);
            self.faults.remove(at);
        }
        Some(reply)
    }

    pub(crate) fn remove_fault(&mut self, id: &str) -> bool {
        let Some(at) = self.faults.iter().position(|stored| stored.id == id) else {
            return false;
        };
        let stored = self.faults.remove(at);
        self.fault_bytes = self.fault_bytes.saturating_sub(fault_bytes(&stored.fault));
        true
    }

    pub(crate) fn clear_faults(&mut self) {
        self.faults.clear();
        self.fault_bytes = 0;
    }

    pub(crate) fn faults_remaining(&self) -> usize {
        self.faults.iter().fold(0usize, |sum, stored| {
            sum.saturating_add(stored.fault.remaining)
        })
    }

    pub(crate) fn faults(&self) -> Vec<FaultInfo> {
        self.faults
            .iter()
            .map(|stored| FaultInfo {
                id: stored.id.clone(),
                scope: stored.scope.clone(),
                action: stored.fault.action.clone(),
                kind: reply_kind(&stored.fault.reply),
                remaining: stored.fault.remaining,
            })
            .collect()
    }

    /// Stores or replaces the rule of (`scope.station`, `action`); capped.
    pub(crate) fn set_rule(
        &mut self,
        scope: &Scope,
        action: &str,
        payload: Value,
    ) -> Result<(), String> {
        let key = (scope.station.clone(), action.to_owned());
        let bytes = payload
            .to_string()
            .len()
            .saturating_add(action.len())
            .saturating_add(scope.station.as_ref().map_or(0, String::len));
        let replaced = self.rules.get(&key).map_or(0, |rule| rule.bytes);
        if replaced == 0 {
            if self.rules.len() >= MAX_SCOPED_RULES {
                return Err(format!(
                    "too many scoped rules stored (at most {MAX_SCOPED_RULES})"
                ));
            }
            if let Some(station) = &scope.station {
                let held = self
                    .rules
                    .keys()
                    .filter(|(held, _)| held.as_ref() == Some(station))
                    .count();
                if held >= MAX_RULES_PER_STATION {
                    return Err(format!(
                        "too many rules stored for one station (at most {MAX_RULES_PER_STATION})"
                    ));
                }
            }
        }
        let total = self
            .rule_bytes
            .saturating_sub(replaced)
            .saturating_add(bytes);
        if total > MAX_RULE_BYTES {
            return Err(format!(
                "stored scoped rules would exceed {} MiB",
                MAX_RULE_BYTES / (1024 * 1024)
            ));
        }
        self.rule_bytes = total;
        self.rules.insert(
            key,
            StoredRule {
                protocol: scope.protocol.clone(),
                payload,
                bytes,
            },
        );
        Ok(())
    }

    pub(crate) fn clear_rule(&mut self, station: Option<&str>, action: &str) -> bool {
        let key = (station.map(str::to_owned), action.to_owned());
        match self.rules.remove(&key) {
            Some(rule) => {
                self.rule_bytes = self.rule_bytes.saturating_sub(rule.bytes);
                true
            }
            None => false,
        }
    }

    /// Clears the global rule for `action` only when it names `protocol`.
    pub(crate) fn clear_global_rule_for_protocol(&mut self, action: &str, protocol: &str) -> bool {
        let key = (None, action.to_owned());
        let matches = self
            .rules
            .get(&key)
            .is_some_and(|rule| rule.protocol.as_deref() == Some(protocol));
        matches && self.clear_rule(None, action)
    }

    pub(crate) fn clear_rules(&mut self) {
        self.rules.clear();
        self.rule_bytes = 0;
    }

    /// The payload of the rule that answers `action` from `station`: the station's own rule
    /// first, then a global rule that names a protocol.
    pub(crate) fn rule_payload(
        &self,
        station: &str,
        protocol: Option<&str>,
        action: &str,
    ) -> Option<Value> {
        let find = |owner: Option<&str>| {
            let key = (owner.map(str::to_owned), action.to_owned());
            self.rules
                .get(&key)
                .filter(|rule| {
                    rule.protocol
                        .as_deref()
                        .is_none_or(|wanted| Some(wanted) == protocol)
                })
                .map(|rule| rule.payload.clone())
        };
        find(Some(station)).or_else(|| find(None))
    }

    /// The rules of one scope owner (`Some(station)`), or of every owner (`None`), sorted by
    /// station then action.
    pub(crate) fn rules(&self, station: Option<&str>) -> Vec<RuleInfo> {
        let mut list: Vec<RuleInfo> = self
            .rules
            .iter()
            .filter(|((owner, _), _)| station.is_none_or(|id| owner.as_deref() == Some(id)))
            .map(|((owner, action), rule)| RuleInfo {
                scope: Scope {
                    station: owner.clone(),
                    protocol: rule.protocol.clone(),
                },
                action: action.clone(),
                payload: rule.payload.clone(),
            })
            .collect();
        list.sort_by(|a, b| (&a.scope.station, &a.action).cmp(&(&b.scope.station, &b.action)));
        list
    }

    pub(crate) fn rule(&self, station: &str, action: &str) -> Option<RuleInfo> {
        let key = (Some(station.to_owned()), action.to_owned());
        self.rules.get(&key).map(|rule| RuleInfo {
            scope: Scope {
                station: Some(station.to_owned()),
                protocol: rule.protocol.clone(),
            },
            action: action.to_owned(),
            payload: rule.payload.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn station_fault_beats_global_and_protocol_mismatch_is_ignored() {
        let mut s = Scripted::default();
        s.add_fault(Fault::silence("Heartbeat"), Scope::global(), true)
            .unwrap();
        s.add_fault(Fault::close("Heartbeat"), Scope::station("A"), true)
            .unwrap();
        s.add_fault(
            Fault::call_error("Heartbeat"),
            Scope::station("B").protocol("ocpp2.0.1"),
            true,
        )
        .unwrap();
        // B negotiated ocpp1.6: its own fault does not apply, the global one does.
        assert_eq!(
            s.take_fault("B", Some("ocpp1.6"), "Heartbeat"),
            Some(Reply::Silence)
        );
        // A: station fault first even though the global one was injected earlier.
        assert_eq!(
            s.take_fault("A", Some("ocpp1.6"), "Heartbeat"),
            Some(Reply::Close)
        );
        assert_eq!(s.take_fault("A", Some("ocpp1.6"), "Heartbeat"), None);
        assert_eq!(s.faults().len(), 1);
        assert_eq!(s.faults_remaining(), 1);
    }

    #[test]
    fn counts_are_per_fault_and_ids_are_unique() {
        let mut s = Scripted::default();
        let a = s
            .add_fault(Fault::silence("X").times(2), Scope::station("A"), true)
            .unwrap();
        let b = s
            .add_fault(Fault::silence("X").times(2), Scope::station("A"), true)
            .unwrap();
        assert_ne!(a, b);
        assert!(s.take_fault("A", None, "X").is_some());
        assert!(s.take_fault("A", None, "X").is_some());
        // The first fault is used up; the second is untouched.
        let left = s.faults();
        assert_eq!(left.len(), 1);
        assert_eq!(
            left.first().map(|f| (f.id.as_str(), f.remaining)),
            Some((b.as_str(), 2))
        );
        assert!(s.remove_fault(&b));
        assert!(!s.remove_fault(&b));
        assert_eq!(s.fault_bytes, 0);
    }

    #[test]
    fn rule_resolution_and_caps() {
        let mut s = Scripted::default();
        s.set_rule(
            &Scope::global().protocol("ocpp1.6"),
            "Heartbeat",
            json!({"g": 1}),
        )
        .unwrap();
        s.set_rule(&Scope::station("A"), "Heartbeat", json!({"a": 1}))
            .unwrap();
        assert_eq!(
            s.rule_payload("A", Some("ocpp1.6"), "Heartbeat"),
            Some(json!({"a": 1}))
        );
        assert_eq!(
            s.rule_payload("B", Some("ocpp1.6"), "Heartbeat"),
            Some(json!({"g": 1}))
        );
        assert_eq!(s.rule_payload("B", None, "Heartbeat"), None);
        assert!(s.clear_rule(Some("A"), "Heartbeat"));
        assert!(!s.clear_rule(Some("A"), "Heartbeat"));
        for i in 0..MAX_RULES_PER_STATION {
            s.set_rule(&Scope::station("C"), &format!("Action{i}"), json!({}))
                .unwrap();
        }
        assert!(s.set_rule(&Scope::station("C"), "One", json!({})).is_err());
        // Replacing an existing rule is not a new rule.
        s.set_rule(&Scope::station("C"), "Action0", json!({"x": 1}))
            .unwrap();
        s.clear_rules();
        assert_eq!(s.rule_bytes, 0);
    }

    #[test]
    fn per_station_fault_cap() {
        let mut s = Scripted::default();
        for _ in 0..MAX_FAULTS_PER_STATION {
            s.add_fault(Fault::silence("X"), Scope::station("A"), true)
                .unwrap();
        }
        assert!(
            s.add_fault(Fault::silence("X"), Scope::station("A"), true)
                .is_err()
        );
        s.add_fault(Fault::silence("X"), Scope::station("B"), true)
            .unwrap();
        // The uncapped path (terminal UI) stores anyway.
        s.add_fault(Fault::silence("X"), Scope::station("A"), false)
            .unwrap();
    }
}
