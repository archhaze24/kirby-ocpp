//! The built-in scenarios, ported from the TS TUI (`chargeSessionScenario`,
//! `finishSessionScenario`, `offlineSyncScenario`, `faultRecoveryScenario`).
//! `heartbeat-and-status`, `metering-burst`, `data-transfer`, `security-event`
//! and `reconnect` are new in 0.5.
//! They are embedded TOML files, so `scenarios show` prints exactly what runs.

use std::borrow::Cow;

use super::{Scenario, ScenarioError};
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

/// `(name, TOML source)` of every OCPP 2.0.1 built-in. There is no `finish-session`: it needs a
/// transaction that is already running.
pub const SOURCES_V201: &[(&str, &str)] = &[
    (
        "charge-session",
        include_str!("../../scenarios/v201/charge-session.toml"),
    ),
    (
        "offline-sync",
        include_str!("../../scenarios/v201/offline-sync.toml"),
    ),
    (
        "fault-recovery",
        include_str!("../../scenarios/v201/fault-recovery.toml"),
    ),
    (
        "heartbeat-and-status",
        include_str!("../../scenarios/v201/heartbeat-and-status.toml"),
    ),
    (
        "metering-burst",
        include_str!("../../scenarios/v201/metering-burst.toml"),
    ),
    (
        "data-transfer",
        include_str!("../../scenarios/v201/data-transfer.toml"),
    ),
    (
        "security-event",
        include_str!("../../scenarios/v201/security-event.toml"),
    ),
    (
        "reconnect",
        include_str!("../../scenarios/v201/reconnect.toml"),
    ),
];

/// The file line of a 2.0.1 built-in that declares its version.
const V201_DECLARATION: &str = "ocpp_version = \"2.0.1\"";

/// The text of a built-in as `protocol` runs it: the embedded file, with the 2.0.1 declaration
/// of the 2.x files replaced for 2.1 (the 2.1 built-ins are the 2.0.1 ones, run as 2.1).
fn text_for(text: &'static str, protocol: Protocol) -> Cow<'static, str> {
    if protocol == Protocol::V21 {
        let declaration = format!("ocpp_version = \"{}\"", Protocol::V21.ocpp_version());
        Cow::Owned(text.replacen(V201_DECLARATION, &declaration, 1))
    } else {
        Cow::Borrowed(text)
    }
}

fn parse(name: &str, text: &'static str, protocol: Protocol) -> Result<Scenario, ScenarioError> {
    let mut scenario = Scenario::from_toml(&text_for(text, protocol))?;
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

/// The embedded built-in files of `protocol`. OCPP 2.1 runs the 2.0.1 files.
fn sources_for(protocol: Protocol) -> &'static [(&'static str, &'static str)] {
    match protocol {
        Protocol::V16 => SOURCES,
        Protocol::V201 | Protocol::V21 => SOURCES_V201,
    }
}

/// All built-in scenarios of `protocol`, parsed.
///
/// # Panics
/// Never for the shipped files (a unit test parses them).
pub fn all_for(protocol: Protocol) -> Vec<Scenario> {
    sources_for(protocol)
        .iter()
        .map(|(name, text)| {
            parse(name, text, protocol).unwrap_or_else(|e| panic!("built-in `{name}`: {e}"))
        })
        .collect()
}

/// The TOML source of a 1.6 built-in (`charge-session` or `charge_session`).
pub fn source(name: &str) -> Option<&'static str> {
    source_for(name, Protocol::V16)
}

/// The embedded TOML file of the built-in `name` of `protocol`. For OCPP 2.1 that is the
/// 2.0.1 file; [`source_text`] gives the text as 2.1 runs it.
pub fn source_for(name: &str, protocol: Protocol) -> Option<&'static str> {
    let wanted = normalize(name);
    sources_for(protocol)
        .iter()
        .find(|(n, _)| *n == wanted)
        .map(|(_, s)| *s)
}

/// The TOML text of the built-in `name` as `protocol` runs it (what `scenarios show` prints):
/// [`source_for`], with the declared `ocpp_version` set to the version of `protocol`.
pub fn source_text(name: &str, protocol: Protocol) -> Option<Cow<'static, str>> {
    source_for(name, protocol).map(|text| text_for(text, protocol))
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
        .and_then(|(n, text)| parse(n, text, protocol).ok())
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
        let v201 = all_for(Protocol::V201);
        assert_eq!(v201.len(), 8);
        for (scenario, (name, _)) in v201.iter().zip(SOURCES_V201) {
            assert_eq!(scenario.name, *name);
            assert_eq!(scenario.ocpp_version, Some(Protocol::V201));
            assert_ne!(scenario.description, "");
        }
        assert_eq!(
            get_for("charge_session", Protocol::V201).map(|s| s.source),
            Some(Some("builtin:charge-session".to_owned()))
        );
        assert_ne!(
            source_for("charge-session", Protocol::V201),
            source("charge-session")
        );
        assert!(get_for("finish-session", Protocol::V201).is_none());
        assert!(source_for("finish-session", Protocol::V201).is_none());
    }

    #[test]
    fn the_2_1_built_ins_are_the_2_0_1_ones_run_as_2_1() {
        let older_set = all_for(Protocol::V201);
        let newer_set = all_for(Protocol::V21);
        assert_eq!(newer_set.len(), 8);
        for (older, newer) in older_set.iter().zip(&newer_set) {
            assert_eq!(older.name, newer.name);
            assert_eq!(newer.ocpp_version, Some(Protocol::V21));
            assert_eq!(newer.protocol(), Protocol::V21);
            assert_eq!(older.steps, newer.steps, "{}", older.name);
        }
        assert_eq!(
            get_for("charge_session", Protocol::V21).map(|s| s.source),
            Some(Some("builtin:charge-session".to_owned()))
        );
        assert!(get_for("finish-session", Protocol::V21).is_none());
        // The embedded file is shared; the shown text declares 2.1.
        assert_eq!(
            source_for("charge-session", Protocol::V21),
            source_for("charge-session", Protocol::V201)
        );
        let shown = source_text("charge-session", Protocol::V21).expect("text");
        assert!(shown.contains("ocpp_version = \"2.1\""));
        assert!(!shown.contains("2.0.1"));
        let unchanged = source_text("charge-session", Protocol::V201).expect("text");
        assert_eq!(
            unchanged,
            source_for("charge-session", Protocol::V201).expect("source")
        );
    }
}
