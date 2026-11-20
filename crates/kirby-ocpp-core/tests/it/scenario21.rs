//! OCPP 2.1 scenario files: the 2.x grammar under `ocpp_version = "2.1"`, the values and
//! lengths that differ from 2.0.1, and the 2.0.1 behavior that stays.

use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::types::{IdTokenType, StopReason};
use kirby_ocpp_core::scenario::{Action, Action2, Scenario};

fn parse(version: &str, body: &str) -> Result<Scenario, String> {
    Scenario::from_toml(&format!(
        "name = \"t\"\nocpp_version = \"{version}\"\n{body}"
    ))
    .map_err(|e| e.to_string().replace("invalid scenario: ", ""))
}

fn step_of(scenario: &Scenario) -> &Action2 {
    match &scenario.steps[0].action {
        Action::V201(action) => action,
        other => panic!("not a 2.x step: {other:?}"),
    }
}

#[test]
fn a_2_1_file_reads_the_2_x_grammar_and_keeps_its_version() {
    let scenario = parse(
        "2.1",
        "[[steps]]\naction = \"start_tx\"\nevse = 1\nid_token = \"TAG\"\n",
    )
    .expect("a 2.1 file parses");
    assert_eq!(scenario.ocpp_version, Some(Protocol::V21));
    assert_eq!(scenario.protocol(), Protocol::V21);
    assert!(matches!(
        step_of(&scenario),
        Action2::StartTx { evse: 1, id_token, token_type: IdTokenType::Iso14443 } if id_token == "TAG"
    ));
    // 1.6 steps are refused the same way as in a 2.0.1 file, naming the file's version.
    let error = parse(
        "2.1",
        "[[steps]]\naction = \"set_rule\"\nrule = \"Reset=reject\"\n",
    )
    .expect_err("set_rule is 1.6 only");
    assert!(error.contains("not available for OCPP 2.1"), "{error}");
    assert!(!error.contains("2.0.1"), "{error}");
    let error = parse(
        "2.0.1",
        "[[steps]]\naction = \"set_rule\"\nrule = \"Reset=reject\"\n",
    )
    .expect_err("set_rule is 1.6 only");
    assert!(error.contains("not available for OCPP 2.0.1"), "{error}");
}

#[test]
fn a_2_1_file_accepts_a_free_token_kind_and_a_2_0_1_file_keeps_refusing_it() {
    let step = "[[steps]]\naction = \"authorize\"\nid_token = \"A\"\ntype = \"Plate\"\n";
    let scenario = parse("2.1", step).expect("2.1 takes any kind");
    assert!(matches!(
        step_of(&scenario),
        Action2::Authorize { token_type: IdTokenType::Other(kind), .. } if kind == "Plate"
    ));
    // A known kind still maps to its variant.
    let known = parse(
        "2.1",
        "[[steps]]\naction = \"authorize\"\nid_token = \"A\"\ntype = \"MacAddress\"\n",
    )
    .expect("a known kind");
    assert!(matches!(
        step_of(&known),
        Action2::Authorize {
            token_type: IdTokenType::MacAddress,
            ..
        }
    ));
    let error = parse("2.0.1", step).expect_err("2.0.1 has the enumeration");
    assert_eq!(
        error,
        "step 1: action `authorize`: unknown variant `Plate`, expected one of `Central`, `eMAID`, `ISO14443`, `ISO15693`, `KeyCode`, `Local`, `MacAddress`, `NoAuthorization` (line 3, column 1)"
    );
}

#[test]
fn a_2_1_stop_reason_is_refused_by_a_2_0_1_file() {
    let step =
        "[[steps]]\naction = \"stop_tx\"\nevse = 1\nreason = \"ReqEnergyTransferRejected\"\n";
    let scenario = parse("2.1", step).expect("2.1 has the reason");
    assert!(matches!(
        step_of(&scenario),
        Action2::StopTx {
            reason: StopReason::ReqEnergyTransferRejected,
            ..
        }
    ));
    let error = parse("2.0.1", step).expect_err("2.0.1 lacks the reason");
    assert!(
        error.contains(
            "unknown variant `ReqEnergyTransferRejected`, expected one of `DeAuthorized`"
        ),
        "{error}"
    );
    // The list the error shows is the 2.0.1 one.
    let listed = error.split("expected one of ").nth(1).expect("a list");
    assert!(
        !listed.contains("ReqEnergyTransferRejected"),
        "the list names a 2.1 value: {error}"
    );
}

#[test]
fn the_id_token_limit_follows_the_version() {
    let long = "T".repeat(100);
    let step = |token: &str| format!("[[steps]]\naction = \"authorize\"\nid_token = \"{token}\"\n");
    parse("2.1", &step(&long)).expect("2.1 allows 255 characters");
    parse("2.1", &step(&"T".repeat(255))).expect("255 characters");
    let error = parse("2.1", &step(&"T".repeat(256))).expect_err("256 is too long");
    assert!(
        error.contains("id_token") && error.contains("255"),
        "{error}"
    );
    let error = parse("2.0.1", &step(&long)).expect_err("2.0.1 stops at 36");
    assert!(
        error.contains("id_token") && error.contains("36"),
        "{error}"
    );
}

#[test]
fn a_reply_step_checks_the_catalog_of_the_file_version() {
    let step = "[[steps]]\naction = \"reply\"\ncall = \"AFRRSignal\"\n[steps.result]\nstatus = \"Accepted\"\n";
    // `AFRRSignal` is a CSMS action of 2.1 only.
    let error = parse("2.0.1", step).expect_err("not a 2.0.1 action");
    assert!(error.contains("unknown OCPP 2.0.1 action"), "{error}");
    parse("2.1", step).expect("a 2.1 action");
}

#[test]
fn a_2_1_file_declares_nothing_but_the_known_versions() {
    let error = parse("2.2", "[[steps]]\naction = \"heartbeat\"\n").expect_err("unknown version");
    assert!(error.contains("OCPP 1.6, 2.0.1 and 2.1 only"), "{error}");
}
