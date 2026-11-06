//! The built-in scenarios, ported from the TS TUI (`chargeSessionScenario`,
//! `finishSessionScenario`, `offlineSyncScenario`, `faultRecoveryScenario`).
//! `heartbeat-and-status`, `metering-burst`, `data-transfer`, `security-event`
//! and `reconnect` are new in 0.5.
//! They are embedded TOML files, so `scenarios show` prints exactly what runs.

use super::Scenario;

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

/// All built-in scenarios, parsed.
///
/// # Panics
/// Never for the shipped files (a unit test parses them).
pub fn all() -> Vec<Scenario> {
    SOURCES
        .iter()
        .map(|(name, text)| {
            Scenario::from_toml(text).unwrap_or_else(|e| panic!("built-in `{name}`: {e}"))
        })
        .collect()
}

fn normalize(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

/// The TOML source of a built-in (`charge-session` or `charge_session`).
pub fn source(name: &str) -> Option<&'static str> {
    let wanted = normalize(name);
    SOURCES.iter().find(|(n, _)| *n == wanted).map(|(_, s)| *s)
}

/// A built-in scenario by name.
pub fn get(name: &str) -> Option<Scenario> {
    source(name).and_then(|text| Scenario::from_toml(text).ok())
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
}
