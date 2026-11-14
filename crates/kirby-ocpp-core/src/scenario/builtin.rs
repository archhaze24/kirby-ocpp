//! The built-in scenarios, ported from the TS TUI (`chargeSessionScenario`,
//! `finishSessionScenario`, `offlineSyncScenario`, `faultRecoveryScenario`).
//! `heartbeat-and-status`, `metering-burst`, `data-transfer`, `security-event`
//! and `reconnect` are new in 0.5.
//! They are embedded TOML files, so `scenarios show` prints exactly what runs.

use super::Scenario;
use crate::ocpp::Protocol;

/// `(name, TOML source)` of every built-in.
pub const SOURCES: &[(&str, &str)] = &[
    (
        "charge-session",
        include_str!("../../scenarios/charge-session.toml"),
    ),
    (
        "finish-session",
        include_str!("../../scenarios/finish-session.toml"),
    ),
    (
        "offline-sync",
        include_str!("../../scenarios/offline-sync.toml"),
    ),
    (
        "fault-recovery",
        include_str!("../../scenarios/fault-recovery.toml"),
    ),
    (
        "heartbeat-and-status",
        include_str!("../../scenarios/heartbeat-and-status.toml"),
    ),
    (
        "metering-burst",
        include_str!("../../scenarios/metering-burst.toml"),
    ),
    (
        "data-transfer",
        include_str!("../../scenarios/data-transfer.toml"),
    ),
    (
        "security-event",
        include_str!("../../scenarios/security-event.toml"),
    ),
    ("reconnect", include_str!("../../scenarios/reconnect.toml")),
];

/// `(name, TOML source)` of every OCPP 2.0.1 built-in. Empty until the 2.0.1 scenarios exist.
pub const SOURCES_V201: &[(&str, &str)] = &[];

fn parse(name: &str, text: &str) -> Result<Scenario, super::ScenarioError> {
    let mut scenario = Scenario::from_toml(text)?;
    scenario.source = Some(format!("builtin:{name}"));
    Ok(scenario)
}

/// All built-in scenarios for OCPP 1.6, parsed. See [`all_for`] for the other versions.
///
/// # Panics
/// Never for the shipped files (a unit test parses them).
pub fn all() -> Vec<Scenario> {
    all_for(Protocol::V16)
}

fn normalize(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

/// The built-in sources of `protocol`; none for a version without scenarios.
fn sources_for(protocol: Protocol) -> &'static [(&'static str, &'static str)] {
    match protocol {
        Protocol::V16 => SOURCES,
        Protocol::V201 => SOURCES_V201,
        _ => &[],
    }
}

/// All built-in scenarios of `protocol`, parsed.
///
/// # Panics
/// Never for the shipped files (a unit test parses them).
pub fn all_for(protocol: Protocol) -> Vec<Scenario> {
    sources_for(protocol)
        .iter()
        .map(|(name, text)| parse(name, text).unwrap_or_else(|e| panic!("built-in `{name}`: {e}")))
        .collect()
}

/// The TOML source of a 1.6 built-in (`charge-session` or `charge_session`).
pub fn source(name: &str) -> Option<&'static str> {
    source_for(name, Protocol::V16)
}

/// The TOML source of the built-in `name` of `protocol`.
pub fn source_for(name: &str, protocol: Protocol) -> Option<&'static str> {
    let wanted = normalize(name);
    sources_for(protocol)
        .iter()
        .find(|(n, _)| *n == wanted)
        .map(|(_, s)| *s)
}

/// A 1.6 built-in scenario by name.
pub fn get(name: &str) -> Option<Scenario> {
    get_for(name, Protocol::V16)
}

/// The built-in scenario `name` of `protocol`.
pub fn get_for(name: &str, protocol: Protocol) -> Option<Scenario> {
    let wanted = normalize(name);
    sources_for(protocol)
        .iter()
        .find(|(n, _)| *n == wanted)
        .and_then(|(n, text)| parse(n, text).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse_and_are_named_after_their_key() {
        let all = all();
        assert_eq!(all.len(), 9);
        for (scenario, (name, _)) in all.iter().zip(SOURCES) {
            assert_eq!(scenario.name, *name);
            assert_ne!(scenario.description, "");
        }
        assert!(get("charge_session").is_some());
        assert!(get("nope").is_none());
    }

    #[test]
    fn built_ins_are_looked_up_per_protocol() {
        assert_eq!(all_for(Protocol::V16), all());
        assert_eq!(
            get_for("charge-session", Protocol::V16),
            get("charge-session")
        );
        assert_eq!(
            source_for("charge_session", Protocol::V16),
            source("charge-session")
        );
        // No 2.0.1 built-ins yet; the lookups are empty rather than falling back to 1.6.
        assert_eq!(all_for(Protocol::V201).len(), 0);
        assert!(get_for("charge-session", Protocol::V201).is_none());
        assert!(source_for("charge-session", Protocol::V201).is_none());
    }
}
