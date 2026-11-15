//! OCPP 2.0.1 scenario files: parsing, validation, labels and retargeting, and the check that
//! 1.6 files read as before.

use std::path::{Path, PathBuf};

use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, IdTokenType, StopReason};
use kirby_ocpp_core::scenario::{
    Action, Action2, Condition2, Scenario, Step, VariableRef, builtin,
};

const HEAD: &str = "name = \"t\"\nocpp_version = \"2.0.1\"\n";

fn parse(body: &str) -> Result<Scenario, String> {
    Scenario::from_toml(&format!("{HEAD}{body}"))
        .map_err(|e| e.to_string().replace("invalid scenario: ", ""))
}

fn ok(body: &str) -> Scenario {
    parse(body).unwrap_or_else(|e| panic!("{e}\n{body}"))
}

/// The error of one `[[steps]]` table (its text starts at line 3).
fn step_err(step: &str) -> String {
    parse(&format!("[[steps]]\n{step}\n")).expect_err(step)
}

fn v201(step: &Step) -> &Action2 {
    match &step.action {
        Action::V201(action) => action,
        other => panic!("not a 2.0.1 step: {other:?}"),
    }
}

// ---- 1.6 files read as before -------------------------------------------------------

fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    files
}

fn no_v201(steps: &[Step]) {
    for step in steps {
        match &step.action {
            Action::V201(action) => panic!("a 1.6 file produced the 2.0.1 step {action:?}"),
            Action::Repeat { steps, .. } => no_v201(steps),
            _ => {}
        }
    }
}

#[test]
fn every_1_6_builtin_and_family_file_parses_as_a_1_6_scenario() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = toml_files(&root.join("scenarios"));
    let builtin_count = files.len();
    files.extend(toml_files(&root.join("../../conformance/scenarios")));
    assert_eq!(builtin_count, builtin::all().len());
    assert!(
        files.len() >= builtin_count + 15,
        "family files are missing"
    );
    let mut checked = 0;
    for path in files {
        let text = std::fs::read_to_string(&path).expect("readable file");
        if text.contains("ocpp_version = \"2.0.1\"") {
            continue;
        }
        let plain =
            Scenario::from_toml(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        // The explicit 1.6 header selects the same grammar: the scenarios are equal apart
        // from the recorded `ocpp_version`.
        let explicit = Scenario::from_toml(&format!("ocpp_version = \"1.6\"\n{text}"))
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(plain.ocpp_version, None, "{}", path.display());
        assert_eq!(explicit.ocpp_version, Some(Protocol::V16));
        let mut explicit = explicit;
        explicit.ocpp_version = None;
        assert_eq!(plain, explicit, "{}", path.display());
        assert_eq!(plain.protocol(), Protocol::V16);
        no_v201(&plain.steps);
        checked += 1;
    }
    assert!(checked >= 24, "only {checked} files were checked");
    for scenario in builtin::all() {
        let source = builtin::source(&scenario.name).expect("builtin source");
        let mut again = Scenario::from_toml(source).expect("builtin parses");
        again.source.clone_from(&scenario.source);
        assert_eq!(scenario, again, "{}", scenario.name);
    }
}

// ---- the 2.0.1 grammar ----------------------------------------------------------------

const EVERY_STEP: &str = r#"
[[steps]]
action = "connect"
[[steps]]
action = "boot"
[[steps]]
action = "plug"
evse = 1
[[steps]]
action = "authorize"
id_token = "TAG"
[[steps]]
action = "start_session"
evse = 1
id_token = "TAG"
type = "KeyCode"
[[steps]]
action = "present_token"
id_token = "TAG"
[[steps]]
action = "meter"
evse = 1
[[steps]]
action = "charging_state"
evse = 1
state = "SuspendedEV"
[[steps]]
action = "stop_session"
evse = 1
[[steps]]
action = "fault"
evse = 1
connector = 1
[[steps]]
action = "clear_fault"
evse = 1
[[steps]]
action = "status_notification"
evse = 1
connector = 1
[[steps]]
action = "security_event"
event_type = "MyOwnEvent"
[[steps]]
action = "data_transfer"
vendor_id = "V"
data = { a = [1, 2] }
[[steps]]
action = "wait_for"
condition = { kind = "tx_idle", evse = 1 }
[[steps]]
action = "wait_for"
condition = { kind = "connector_status", evse = 1, connector = 1, status = "Available" }
timeout_ms = 100
[[steps]]
action = "unplug"
evse = 1
[[steps]]
action = "disconnect"
"#;

#[test]
fn a_2_0_1_file_reads_every_step_with_its_defaults() {
    let s = ok(EVERY_STEP);
    assert_eq!(s.ocpp_version, Some(Protocol::V201));
    assert_eq!(s.protocol(), Protocol::V201);
    let a = |i: usize| v201(&s.steps[i]);
    assert!(matches!(&s.steps[0].action, Action::Connect {}));
    assert!(matches!(a(2), Action2::Plug { evse: 1 }));
    assert!(matches!(
        a(3),
        Action2::Authorize { id_token, token_type: IdTokenType::Iso14443 } if id_token == "TAG"
    ));
    assert!(matches!(
        a(4),
        Action2::StartTx {
            evse: 1,
            token_type: IdTokenType::KeyCode,
            ..
        }
    ));
    assert!(matches!(a(5), Action2::PresentToken { evse: None, .. }));
    assert!(matches!(
        a(6),
        Action2::Meter {
            evse: 1,
            wh_delta: 1000
        }
    ));
    assert!(matches!(
        a(7),
        Action2::ChargingState {
            state: ChargingState::SuspendedEv,
            ..
        }
    ));
    assert!(matches!(
        a(8),
        Action2::StopTx {
            reason: StopReason::Local,
            ..
        }
    ));
    assert!(matches!(
        a(9),
        Action2::Fault {
            evse: 1,
            connector: Some(1)
        }
    ));
    assert!(matches!(
        a(10),
        Action2::ClearFault {
            evse: 1,
            connector: None
        }
    ));
    assert!(matches!(
        a(11),
        Action2::StatusNotification {
            evse: 1,
            connector: 1
        }
    ));
    assert!(matches!(
        a(12),
        Action2::SecurityEvent {
            tech_info: None,
            ..
        }
    ));
    assert!(matches!(
        a(13),
        Action2::DataTransfer { data: Some(data), message_id: None, .. } if data["a"][1] == 2
    ));
    assert!(matches!(
        a(14),
        Action2::WaitFor {
            condition: Condition2::TxIdle { evse: 1 },
            timeout_ms: 5000
        }
    ));
    assert!(matches!(
        a(15),
        Action2::WaitFor {
            timeout_ms: 100,
            ..
        }
    ));
    assert_eq!(a(4).name(), "start_tx");
    assert_eq!(a(8).name(), "stop_tx");
}

#[test]
fn the_example_in_the_docs_parses() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/scenarios.md");
    let text = std::fs::read_to_string(docs).expect("docs page");
    let section = text
        .split("### OCPP 2.0.1 scenarios")
        .nth(1)
        .expect("2.0.1 section");
    let example = section
        .split("```toml\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("toml example");
    let scenario = Scenario::from_toml(example).expect("the documented example loads");
    assert_eq!(scenario.protocol(), Protocol::V201);
    assert_eq!(scenario.steps.len(), 6);
}

#[test]
fn every_wait_for_condition_parses() {
    for (condition, expected) in [
        ("{ kind = \"connected\" }", "Connected"),
        ("{ kind = \"disconnected\" }", "Disconnected"),
        (
            "{ kind = \"registration\", status = \"Pending\" }",
            "Registration",
        ),
        ("{ kind = \"tx_active\", evse = 2 }", "TxActive"),
        (
            "{ kind = \"charging_state\", evse = 1, state = \"Charging\" }",
            "ChargingState",
        ),
        ("{ kind = \"no_pending_messages\" }", "NoPendingMessages"),
    ] {
        let s = ok(&format!(
            "[[steps]]\naction = \"wait_for\"\ncondition = {condition}\n"
        ));
        let Action2::WaitFor { condition, .. } = v201(&s.steps[0]) else {
            panic!("not a wait_for");
        };
        assert!(
            format!("{condition:?}").starts_with(expected),
            "{condition:?}"
        );
    }
}

#[test]
fn set_variable_takes_a_name_or_a_table() {
    let s = ok(r#"
[[steps]]
action = "set_variable"
component = "OCPPCommCtrlr"
variable = "HeartbeatInterval"
value = 30
[[steps]]
action = "set_variable"
component = { name = "Connector", instance = "1", evse = 2 }
variable = { name = "Enabled", instance = "x" }
value = true
[[steps]]
action = "set_variable"
component = "C"
variable = "V"
value = "text"
"#);
    let Action2::SetVariable {
        component,
        variable,
        value,
    } = v201(&s.steps[0])
    else {
        panic!("not set_variable");
    };
    assert_eq!(
        (
            component.name.as_str(),
            variable.name.as_str(),
            value.as_str()
        ),
        ("OCPPCommCtrlr", "HeartbeatInterval", "30")
    );
    assert!(component.instance.is_none() && component.evse.is_none());
    let Action2::SetVariable {
        component,
        variable,
        value,
    } = v201(&s.steps[1])
    else {
        panic!("not set_variable");
    };
    assert_eq!(component.evse, Some(2));
    assert_eq!(component.instance.as_deref(), Some("1"));
    assert_eq!(
        variable,
        &VariableRef {
            name: "Enabled".to_owned(),
            instance: Some("x".to_owned())
        }
    );
    assert_eq!(value, "true");
    assert!(matches!(v201(&s.steps[2]), Action2::SetVariable { value, .. } if value == "text"));
}

#[test]
fn id_tokens_may_use_placeholders_and_no_authorization_may_be_empty() {
    ok("[[steps]]\naction = \"authorize\"\nid_token = \"${tag}\"\n");
    ok("[[steps]]\naction = \"authorize\"\nid_token = \"\"\ntype = \"NoAuthorization\"\n");
    // A 36 character token is the longest.
    ok(&format!(
        "[[steps]]\naction = \"authorize\"\nid_token = \"{}\"\n",
        "A".repeat(36)
    ));
}

#[test]
fn repeat_nests_2_0_1_steps_and_labels_carry_the_iteration() {
    let s = ok(r#"
[[steps]]
action = "repeat"
count = 2
[[steps.steps]]
action = "start_tx"
evse = 1
id_token = "TAG"
[[steps.steps]]
action = "stop_tx"
evse = 1
"#);
    let flat = s.expanded_steps();
    let labels: Vec<_> = flat.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "start_tx e1 TAG [1]",
            "stop_tx e1 [1]",
            "start_tx e1 TAG [2]",
            "stop_tx e1 [2]"
        ]
    );
    assert_eq!(s.expanded_len(), 4);
}

#[test]
fn stable_labels_name_the_evse_and_the_token() {
    let s = ok(r#"
[[steps]]
action = "plug"
evse = 1
[[steps]]
action = "start_tx"
evse = 2
id_token = "TAG"
[[steps]]
action = "authorize"
id_token = "TAG"
[[steps]]
action = "stop_tx"
evse = 1
[[steps]]
action = "charging_state"
evse = 1
state = "Charging"
[[steps]]
action = "status_notification"
evse = 1
connector = 2
[[steps]]
action = "wait_for"
condition = { kind = "tx_active", evse = 1 }
[[steps]]
action = "wait_for"
condition = { kind = "charging_state", evse = 1, state = "Idle" }
[[steps]]
action = "set_variable"
component = "SecurityCtrlr"
variable = "BasicAuthPassword"
value = "never-in-a-label"
[[steps]]
action = "security_event"
event_type = "Custom"
[[steps]]
action = "data_transfer"
vendor_id = "V"
[[steps]]
name = "named"
action = "plug"
evse = 1
[[steps]]
action = "heartbeat"
"#);
    let labels: Vec<_> = s.steps.iter().map(Step::stable_label).collect();
    assert_eq!(
        labels,
        [
            "plug e1",
            "start_tx e2 TAG",
            "authorize TAG",
            "stop_tx e1",
            "charging_state e1 Charging",
            "status_notification e1 c2",
            "wait_for tx_active e1",
            "wait_for charging_state e1 Idle",
            "set_variable SecurityCtrlr BasicAuthPassword",
            "security_event Custom",
            "data_transfer V",
            "named",
            "heartbeat",
        ]
    );
}

#[test]
fn capture_is_valid_on_the_token_steps() {
    for action in ["authorize", "start_tx", "present_token"] {
        let evse = if action == "authorize" {
            ""
        } else {
            "evse = 1\n"
        };
        ok(&format!(
            "[[steps]]\naction = \"{action}\"\n{evse}id_token = \"T\"\ncapture = {{ tx = \"/transaction_id\" }}\n"
        ));
    }
    ok("[[steps]]\naction = \"data_transfer\"\nvendor_id = \"V\"\ncapture = { a = \"/data\" }\n");
}

#[test]
fn expect_works_on_2_0_1_steps() {
    ok("[[steps]]\naction = \"authorize\"\nid_token = \"T\"\nexpect = { status = \"Blocked\" }\n");
    let e = step_err(
        "action = \"authorize\"\nid_token = \"T\"\nexpect = { error = true, status = \"Blocked\" }",
    );
    assert!(
        e.contains("`error = true` excludes `status` and `payload`"),
        "{e}"
    );
}

#[test]
fn reply_is_checked_for_2_0_1() {
    ok("[[steps]]\naction = \"reply\"\ncall = \"reset\"\nresult = { status = \"Rejected\" }\n");
    ok("[[steps]]\naction = \"reply\"\ncall = \"Reset\"\nresult = { status = \"${s}\" }\n");
    ok("[[steps]]\naction = \"reply\"\ncall = \"Reset\"\nerror = { code = \"formatviolation\" }\n");
    ok(
        "[[steps]]\naction = \"reply\"\ncall = \"DataTransfer\"\nresult = { status = \"Accepted\" }\n",
    );
    // The 1.6 command list does not apply.
    assert_eq!(
        step_err("action = \"reply\"\ncall = \"ChangeConfiguration\"\nresult = {}"),
        "step 1: action `reply`: unknown OCPP 2.0.1 action `ChangeConfiguration` (expected a command the CSMS sends, e.g. Reset)"
    );
    // A 1.6 file keeps its own list.
    let e = Scenario::from_toml(
        "name = \"t\"\n[[steps]]\naction = \"reply\"\ncall = \"ChangeConfiguration\"\nresult = { status = \"Accepted\" }\n",
    );
    assert!(e.is_ok(), "{e:?}");
}

// ---- pinned error texts ---------------------------------------------------------------

#[test]
fn errors_of_1_6_only_steps_say_so_and_give_a_hint() {
    assert_eq!(
        step_err("action = \"set_rule\"\nrule = \"Reset=reject\""),
        "step 1: action `set_rule`: not available for OCPP 2.0.1 (response rules name OCPP 1.6 commands; use a `reply` step to answer a CSMS command) (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"clear_rules\""),
        "step 1: action `clear_rules`: not available for OCPP 2.0.1 (response rules name OCPP 1.6 commands; use a `reply` step to answer a CSMS command) (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"request_certificate\""),
        "step 1: action `request_certificate`: not available for OCPP 2.0.1 (use `send_raw` with `SignCertificate`) (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"status\"\nconnector = 1\nstatus = \"Available\""),
        "step 1: action `status`: not available for OCPP 2.0.1 (`status` sets a 1.6 connector status; use `plug`, `unplug`, `fault` or `clear_fault` to change it and `status_notification` to resend it) (line 3, column 1)"
    );
}

#[test]
fn unknown_steps_get_a_suggestion_from_the_2_0_1_set() {
    let e = step_err("action = \"plog\"\nevse = 1");
    assert!(
        e.starts_with("step 1: action `plog`: unknown variant `plog`, expected one of `connect`, "),
        "{e}"
    );
    assert!(
        e.ends_with("; did you mean `plug`? (line 3, column 1)"),
        "{e}"
    );
    for name in [
        "present_token",
        "status_notification",
        "start_session",
        "wait_for",
    ] {
        assert!(e.contains(&format!("`{name}`")), "{name} missing: {e}");
    }
    // 1.6-only names are not offered.
    assert!(
        !e.contains("`set_rule`") && !e.contains("`request_certificate`"),
        "{e}"
    );
    let e = step_err("action = \"start_sesion\"\nevse = 1\nid_token = \"A\"");
    assert!(e.contains("did you mean `start_session`?"), "{e}");
}

#[test]
fn key_mistakes_name_the_2_0_1_spelling() {
    assert_eq!(
        step_err("action = \"plug\"\nconnector = 1"),
        "step 1: action `plug`: unknown field `connector`, expected `evse`; OCPP 2.0.1 steps use `evse` (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"authorize\"\nid_tag = \"A\""),
        "step 1: action `authorize`: unknown field `id_tag`, expected `id_token` or `type`; OCPP 2.0.1 steps use `id_token` (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"plug\""),
        "step 1: action `plug`: missing field `evse` (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"plug\"\nevse = 1\nextra = 2"),
        "step 1: action `plug`: unknown field `extra`, expected `evse` (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"plug\"\nevse = -1"),
        "step 1: action `plug`: invalid value: integer `-1`, expected u32 (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"status_notification\"\nevse = 1"),
        "step 1: action `status_notification`: missing field `connector` (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"wait_for\"\ncondition = { kind = \"tx_active\", connector = 1 }"),
        "step 1: action `wait_for`: unknown field `connector`, expected `evse`; OCPP 2.0.1 steps use `evse` (line 3, column 1)"
    );
}

#[test]
fn number_ranges_and_timeouts_are_checked_at_load_time() {
    for step in [
        "action = \"plug\"\nevse = 0",
        "action = \"stop_tx\"\nevse = 0",
        "action = \"present_token\"\nevse = 0\nid_token = \"T\"",
        "action = \"wait_for\"\ncondition = { kind = \"tx_idle\", evse = 0 }",
    ] {
        assert_eq!(step_err(step), "step 1: `evse` must be 1 or more", "{step}");
    }
    assert_eq!(
        step_err("action = \"fault\"\nevse = 1\nconnector = 0"),
        "step 1: `connector` must be 1 or more"
    );
    assert_eq!(
        step_err("action = \"status_notification\"\nevse = 1\nconnector = 0"),
        "step 1: `connector` must be 1 or more"
    );
    assert_eq!(
        step_err(
            "action = \"set_variable\"\ncomponent = { name = \"C\", evse = 0 }\nvariable = \"V\"\nvalue = \"x\""
        ),
        "step 1: `component.evse` must be 1 or more"
    );
    assert_eq!(
        step_err(
            "action = \"wait_for\"\ncondition = { kind = \"tx_active\", evse = 1 }\ntimeout_ms = 0"
        ),
        "step 1: `timeout_ms` must be positive"
    );
    assert_eq!(
        step_err(
            "action = \"wait_for\"\ncondition = { kind = \"connected\" }\ntimeout_ms = 86400001"
        ),
        "step 1: `timeout_ms` must be at most 86400000 (one day)"
    );
    assert_eq!(
        step_err("action = \"sleep\"\nms = 86400001"),
        "step 1: `timeout_ms` / `ms` must be at most 86400000 (one day)"
    );
    assert_eq!(
        step_err("action = \"expect_call\"\ncall = \"Heartbeat\"\ntimeout_ms = 0"),
        "step 1: `timeout_ms` must be positive"
    );
    assert_eq!(
        step_err("action = \"repeat\"\ncount = 2\n[[steps.steps]]\naction = \"plug\"\nevse = 0"),
        "step 1.1: `evse` must be 1 or more"
    );
}

#[test]
fn text_lengths_follow_the_schemas() {
    let long = |n: usize| "x".repeat(n);
    for (step, expected) in [
        (
            "action = \"authorize\"\nid_token = \"\"".to_owned(),
            "step 1: `id_token` must be 1 to 36 characters",
        ),
        (
            format!(
                "action = \"start_tx\"\nevse = 1\nid_token = \"{}\"",
                long(37)
            ),
            "step 1: `id_token` must be 1 to 36 characters",
        ),
        (
            "action = \"security_event\"\nevent_type = \"\"".to_owned(),
            "step 1: `event_type` must be 1 to 50 characters",
        ),
        (
            format!("action = \"security_event\"\nevent_type = \"{}\"", long(51)),
            "step 1: `event_type` must be 1 to 50 characters",
        ),
        (
            format!(
                "action = \"security_event\"\nevent_type = \"E\"\ntech_info = \"{}\"",
                long(256)
            ),
            "step 1: `tech_info` must be at most 255 characters",
        ),
        (
            "action = \"data_transfer\"\nvendor_id = \"\"".to_owned(),
            "step 1: `vendor_id` must be 1 to 255 characters",
        ),
        (
            format!(
                "action = \"data_transfer\"\nvendor_id = \"V\"\nmessage_id = \"{}\"",
                long(51)
            ),
            "step 1: `message_id` must be at most 50 characters",
        ),
        (
            format!(
                "action = \"set_variable\"\ncomponent = \"{}\"\nvariable = \"V\"\nvalue = \"x\"",
                long(51)
            ),
            "step 1: `component` must be 1 to 50 characters",
        ),
        (
            "action = \"set_variable\"\ncomponent = \"C\"\nvariable = \"\"\nvalue = \"x\""
                .to_owned(),
            "step 1: `variable` must be 1 to 50 characters",
        ),
        (
            format!(
                "action = \"set_variable\"\ncomponent = \"C\"\nvariable = \"V\"\nvalue = \"{}\"",
                long(2501)
            ),
            "step 1: `value` must be at most 2500 characters",
        ),
    ] {
        assert_eq!(step_err(&step), expected, "{step}");
    }
    // The longest values are accepted.
    ok(&format!(
        "[[steps]]\naction = \"set_variable\"\ncomponent = \"C\"\nvariable = \"V\"\nvalue = \"{}\"\n",
        long(2500)
    ));
    ok(&format!(
        "[[steps]]\naction = \"security_event\"\nevent_type = \"{}\"\n",
        long(50)
    ));
}

#[test]
fn bad_values_and_shapes_have_their_own_messages() {
    assert_eq!(
        step_err("action = \"set_variable\"\ncomponent = 1\nvariable = \"V\"\nvalue = \"x\""),
        "step 1: action `set_variable`: `component` must be a name or a table with `name` (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"set_variable\"\ncomponent = \"C\"\nvariable = \"V\""),
        "step 1: action `set_variable`: missing field `value` (line 3, column 1)"
    );
    assert_eq!(
        step_err(
            "action = \"set_variable\"\ncomponent = { nam = \"C\" }\nvariable = \"V\"\nvalue = \"x\""
        ),
        "step 1: action `set_variable`: unknown field `nam`, expected one of `name`, `instance`, `evse`; did you mean `name`? (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"authorize\"\nid_token = \"A\"\ntype = \"Nope\""),
        "step 1: action `authorize`: unknown variant `Nope`, expected one of `Central`, `eMAID`, `ISO14443`, `ISO15693`, `KeyCode`, `Local`, `MacAddress`, `NoAuthorization` (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"charging_state\"\nevse = 1\nstate = \"Nope\""),
        "step 1: action `charging_state`: unknown variant `Nope`, expected one of `Charging`, `EVConnected`, `SuspendedEV`, `SuspendedEVSE`, `Idle` (line 3, column 1)"
    );
    let e = step_err("action = \"stop_tx\"\nevse = 1\nreason = \"HardReset\"");
    assert!(
        e.contains("unknown variant `HardReset`, expected one of `DeAuthorized`"),
        "{e}"
    );
    assert_eq!(
        step_err("action = \"wait_for\"\ncondition = { kind = \"tx_activ\", evse = 1 }"),
        "step 1: action `wait_for`: unknown variant `tx_activ`, expected one of `connected`, `disconnected`, `registration`, `connector_status`, `tx_active`, `tx_idle`, `charging_state`, `no_pending_messages`; did you mean `tx_active`? (line 3, column 1)"
    );
    assert_eq!(
        step_err("action = \"meter\"\nevse = 1\ncapture = { a = \"/b\" }"),
        "step 1: action `meter`: `capture` is only valid on send_raw, data_transfer, expect_call, authorize, start_tx and present_token steps (line 3, column 1)"
    );
}

#[test]
fn reply_errors_for_2_0_1() {
    assert_eq!(
        step_err("action = \"reply\"\ncall = \"BootNotification\"\nresult = {}"),
        "step 1: action `reply`: `BootNotification` is sent by the station; a `reply` answers a command from the CSMS (e.g. Reset)"
    );
    assert_eq!(
        step_err("action = \"reply\"\ncall = \"Nope\"\nresult = {}"),
        "step 1: action `reply`: unknown OCPP 2.0.1 action `Nope` (expected a command the CSMS sends, e.g. Reset)"
    );
    let e = step_err("action = \"reply\"\ncall = \"Reset\"\nresult = { status = \"Nope\" }");
    assert!(
        e.starts_with("step 1: action `reply`: `result` is not a valid Reset response: Reset response violates OCPP 2.0.1 schema"),
        "{e}"
    );
    let codes = "FormatViolation, GenericError, InternalError, MessageTypeNotSupported, NotImplemented, NotSupported, OccurrenceConstraintViolation, PropertyConstraintViolation, ProtocolError, RpcFrameworkError, SecurityError, TypeConstraintViolation";
    assert_eq!(
        step_err("action = \"reply\"\ncall = \"Reset\"\nerror = { code = \"FormationViolation\" }"),
        format!(
            "step 1: action `reply`: unknown error code `FormationViolation` (spelled `FormatViolation` in OCPP 2.0.1) (expected one of the OCPP 2.0.1 CALLERROR codes: {codes})"
        )
    );
    assert_eq!(
        step_err(
            "action = \"reply\"\ncall = \"Reset\"\nerror = { code = \"OccurenceConstraintViolation\" }"
        ),
        format!(
            "step 1: action `reply`: unknown error code `OccurenceConstraintViolation` (spelled `OccurrenceConstraintViolation` in OCPP 2.0.1) (expected one of the OCPP 2.0.1 CALLERROR codes: {codes})"
        )
    );
    assert_eq!(
        step_err("action = \"reply\"\ncall = \"Reset\"\nerror = { code = \"Nope\" }"),
        format!(
            "step 1: action `reply`: unknown error code `Nope` (expected one of the OCPP 2.0.1 CALLERROR codes: {codes})"
        )
    );
}

#[test]
fn file_level_versions() {
    let step = "[[steps]]\naction = \"heartbeat\"\n";
    let e = Scenario::from_toml(&format!("name = \"t\"\nocpp_version = \"2.1\"\n{step}"))
        .unwrap_err()
        .to_string();
    assert_eq!(
        e,
        "invalid scenario: `ocpp_version = \"2.1\"`: this build supports OCPP 1.6 and 2.0.1 only"
    );
    // The 1.6 grammar still reserves the 2.x keys and points at the version key.
    let e = Scenario::from_toml("name = \"t\"\n[[steps]]\naction = \"plug\"\nevse = 1\n")
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("`evse` is reserved for OCPP 2.x steps; not supported in this build")
            && e.contains("(use `ocpp_version = \"2.0.1\"` for OCPP 2.0.1 scenarios)"),
        "{e}"
    );
    // The 2.0.1 grammar has no 1.6 fallback: 1.6 keys are unknown fields, 1.6 steps are refused.
    let e = Scenario::from_toml(&format!(
        "{HEAD}[[steps]]\naction = \"start_tx\"\nconnector = 1\nid_tag = \"A\"\n"
    ))
    .unwrap_err()
    .to_string();
    assert!(e.contains("unknown field"), "{e}");
}

// ---- retargeting ------------------------------------------------------------------------

#[test]
fn retarget_v201_rewrites_evse_and_token_everywhere() {
    let mut s = ok(r#"
[[steps]]
action = "plug"
evse = 1
[[steps]]
action = "authorize"
id_token = "A"
[[steps]]
action = "start_tx"
evse = 1
id_token = "A"
[[steps]]
action = "present_token"
id_token = "A"
[[steps]]
action = "present_token"
evse = 1
id_token = "A"
[[steps]]
action = "wait_for"
condition = { kind = "connector_status", evse = 1, connector = 1, status = "Occupied" }
[[steps]]
action = "wait_for"
condition = { kind = "tx_active", evse = 1 }
[[steps]]
action = "set_variable"
component = { name = "C", evse = 1 }
variable = "V"
value = "x"
[[steps]]
action = "repeat"
count = 2
[[steps.steps]]
action = "stop_tx"
evse = 1
"#);
    s.retarget_v201(Some(3), Some("OTHER"));
    let a = |i: usize| v201(&s.steps[i]);
    assert!(matches!(a(0), Action2::Plug { evse: 3 }));
    assert!(matches!(a(1), Action2::Authorize { id_token, .. } if id_token == "OTHER"));
    assert!(matches!(a(2), Action2::StartTx { evse: 3, id_token, .. } if id_token == "OTHER"));
    assert!(
        matches!(a(3), Action2::PresentToken { evse: None, id_token, .. } if id_token == "OTHER")
    );
    assert!(matches!(a(4), Action2::PresentToken { evse: Some(3), .. }));
    assert!(matches!(
        a(5),
        Action2::WaitFor {
            condition: Condition2::ConnectorStatus {
                evse: 3,
                connector: 1,
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        a(6),
        Action2::WaitFor {
            condition: Condition2::TxActive { evse: 3 },
            ..
        }
    ));
    assert!(matches!(
        a(7),
        Action2::SetVariable { component, .. } if component.evse == Some(1)
    ));
    let Action::Repeat { steps, .. } = &s.steps[8].action else {
        panic!("not a repeat");
    };
    assert!(matches!(v201(&steps[0]), Action2::StopTx { evse: 3, .. }));

    // Nothing given, nothing changes.
    let before = s.clone();
    s.retarget_v201(None, None);
    assert_eq!(s, before);
}

#[test]
fn retarget_leaves_the_other_grammar_alone() {
    let mut v16 = Scenario::from_toml(
        "name = \"t\"\n[[steps]]\naction = \"start_tx\"\nconnector = 1\nid_tag = \"A\"\n",
    )
    .expect("1.6 scenario");
    let before = v16.clone();
    v16.retarget_v201(Some(2), Some("B"));
    assert_eq!(v16, before);

    let mut v201 = ok("[[steps]]\naction = \"plug\"\nevse = 1\n");
    let before = v201.clone();
    v201.retarget(
        Some(kirby_ocpp_core::ids::ConnectorId::new(2)),
        Some(&kirby_ocpp_core::ids::IdTag::new("B").expect("tag")),
    );
    assert_eq!(v201, before);
}
