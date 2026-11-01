//! Process-level tests of the CLI. They run the real binary because precedence of
//! flags over environment variables cannot be tested in-process without mutating the
//! (shared, and in edition 2024 unsafe to mutate) process environment.

use std::process::{Command, Output};

use serde_json::Value;

const ENV_VARS: &[&str] = &[
    "OCPP_URL",
    "OCPP_CHARGE_POINT_ID",
    "OCPP_VENDOR",
    "OCPP_MODEL",
    "OCPP_WS_SUBPROTOCOL",
    "OCPP_WS_PING_INTERVAL",
    "OCPP_RECONNECT",
    "OCPP_RECONNECT_INITIAL_MS",
    "OCPP_RECONNECT_MAX_MS",
    "OCPP_RECONNECT_MAX_ATTEMPTS",
    "OCPP_CALL_TIMEOUT_MS",
    "OCPP_TLS_CA_FILE",
    "OCPP_TLS_CERT_FILE",
    "OCPP_TLS_KEY_FILE",
    "OCPP_TLS_SERVER_NAME",
    "OCPP_TLS_SKIP_VERIFY",
    "KIRBY_OCPP_NO_PERSIST",
    "KIRBY_OCPP_STATE_DIR",
    "KIRBY_OCPP_RULES",
];

fn run(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-ocpp"));
    for var in ENV_VARS {
        command.env_remove(var);
    }
    command
        .args(args)
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn config(args: &[&str], env: &[(&str, &str)]) -> Value {
    let mut all = vec!["--print-config"];
    all.extend_from_slice(args);
    let output = run(&all, env);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const URL: &[&str] = &["--url", "ws://localhost:9000/ocpp"];

fn with_url<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = URL.to_vec();
    args.extend_from_slice(extra);
    args
}

#[test]
fn version_works_without_a_url() {
    let output = run(&["--version"], &[]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.starts_with("kirby-ocpp "), "{text}");
}

#[test]
fn missing_url_fails_and_mentions_the_env_var() {
    let output = run(&[], &[]);
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("--url"), "{text}");
    assert!(text.contains("OCPP_URL"), "{text}");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn defaults_match_the_ts_cli() {
    let c = config(URL, &[]);
    assert_eq!(c["url"], "ws://localhost:9000/ocpp");
    assert_eq!(c["charge_point_id"], "CP-001");
    assert_eq!(c["vendor"], "archhaze24");
    assert_eq!(c["model"], "KIRBY");
    assert_eq!(c["connector_count"], 1);
    assert_eq!(c["heartbeat_interval_secs"], 30);
    assert_eq!(c["ws_subprotocol"], "ocpp1.6");
    assert_eq!(c["ws_ping_interval_secs"], 30);
    assert_eq!(c["reconnect"]["enabled"], true);
    assert_eq!(c["reconnect"]["initial_delay_ms"], 1000);
    assert_eq!(c["reconnect"]["max_delay_ms"], 30000);
    assert_eq!(c["reconnect"]["max_attempts"], 0);
    assert_eq!(c["call_timeout_ms"], 30000);
    assert_eq!(c["persist"], true);
    assert_eq!(c["tls"]["skip_verify"], false);
    assert_eq!(c["nominal_power_w"], 7200);
    assert_eq!(c["voltage_v"], 230);
    assert_eq!(c["phases"], 1);
}

#[test]
fn power_model_flags_are_mapped_and_validated() {
    let c = config(
        &with_url(&[
            "--nominal-power-w",
            "11000",
            "--voltage-v",
            "400",
            "--phases",
            "3",
        ]),
        &[],
    );
    assert_eq!(c["nominal_power_w"], 11000);
    assert_eq!(c["voltage_v"], 400);
    assert_eq!(c["phases"], 3);
    for extra in [
        vec!["--phases", "4"],
        vec!["--phases", "0"],
        vec!["--voltage-v", "0"],
        vec!["--nominal-power-w", "0"],
    ] {
        let output = run(&with_url(&extra), &[]);
        assert!(!output.status.success(), "{extra:?} should fail");
    }
}

#[test]
fn env_vars_are_used_when_no_flag_is_given() {
    let c = config(
        &[],
        &[
            ("OCPP_URL", "wss://csms.example.com/ocpp"),
            ("OCPP_CHARGE_POINT_ID", "FROM-ENV"),
            ("OCPP_VENDOR", "V"),
            ("OCPP_MODEL", "M"),
            ("OCPP_WS_SUBPROTOCOL", "ocpp1.6"),
            ("OCPP_WS_PING_INTERVAL", "0"),
            ("OCPP_RECONNECT_INITIAL_MS", "10"),
            ("OCPP_RECONNECT_MAX_MS", "20"),
            ("OCPP_RECONNECT_MAX_ATTEMPTS", "3"),
            ("OCPP_CALL_TIMEOUT_MS", "500"),
            ("OCPP_TLS_SERVER_NAME", "sni.example.com"),
            ("KIRBY_OCPP_STATE_DIR", "/tmp/kirby-state"),
        ],
    );
    assert_eq!(c["url"], "wss://csms.example.com/ocpp");
    assert_eq!(c["charge_point_id"], "FROM-ENV");
    assert_eq!(c["vendor"], "V");
    assert_eq!(c["ws_ping_interval_secs"], 0);
    assert_eq!(c["reconnect"]["initial_delay_ms"], 10);
    assert_eq!(c["reconnect"]["max_delay_ms"], 20);
    assert_eq!(c["reconnect"]["max_attempts"], 3);
    assert_eq!(c["call_timeout_ms"], 500);
    assert_eq!(c["tls"]["server_name"], "sni.example.com");
    assert_eq!(c["state_dir"], "/tmp/kirby-state");
}

#[test]
fn cli_flag_beats_env_beats_default() {
    let c = config(
        &with_url(&["--id", "FROM-CLI"]),
        &[
            ("OCPP_CHARGE_POINT_ID", "FROM-ENV"),
            ("OCPP_URL", "ws://ignored/x"),
        ],
    );
    assert_eq!(c["charge_point_id"], "FROM-CLI");
    assert_eq!(c["url"], "ws://localhost:9000/ocpp");
}

#[test]
fn readme_documented_env_booleans_work() {
    let c = config(
        URL,
        &[("OCPP_TLS_SKIP_VERIFY", "1"), ("OCPP_RECONNECT", "0")],
    );
    assert_eq!(c["tls"]["skip_verify"], true);
    assert_eq!(c["reconnect"]["enabled"], false);

    let c = config(URL, &[("KIRBY_OCPP_NO_PERSIST", "1")]);
    assert_eq!(c["persist"], false);
}

#[test]
fn env_booleans_can_be_false_too() {
    let c = config(
        URL,
        &[
            ("OCPP_TLS_SKIP_VERIFY", "false"),
            ("OCPP_RECONNECT", "true"),
            ("KIRBY_OCPP_NO_PERSIST", "0"),
        ],
    );
    assert_eq!(c["tls"]["skip_verify"], false);
    assert_eq!(c["reconnect"]["enabled"], true);
    assert_eq!(c["persist"], true);
}

#[test]
fn garbage_booleans_are_rejected() {
    for (var, value, flag) in [
        ("OCPP_TLS_SKIP_VERIFY", "maybe", "--tls-skip-verify"),
        ("OCPP_RECONNECT", "banana", "--reconnect"),
        ("KIRBY_OCPP_NO_PERSIST", "2", "--no-persist"),
    ] {
        let mut args = vec!["--print-config"];
        args.extend_from_slice(URL);
        let output = run(&args, &[(var, value)]);
        assert!(!output.status.success(), "{var}={value} should fail");
        let text = stderr(&output);
        assert!(
            text.contains("not a boolean") && text.contains(flag),
            "{var}: {text}"
        );
    }
}

#[test]
fn no_reconnect_flag_beats_env_that_enables_it() {
    let c = config(&with_url(&["--no-reconnect"]), &[("OCPP_RECONNECT", "1")]);
    assert_eq!(c["reconnect"]["enabled"], false);
}

#[test]
fn reconnect_flag_beats_env_that_disables_it() {
    let c = config(
        &with_url(&["--reconnect", "true"]),
        &[("OCPP_RECONNECT", "0")],
    );
    assert_eq!(c["reconnect"]["enabled"], true);
}

#[test]
fn tls_flags_and_no_persist_flag() {
    let c = config(
        &with_url(&[
            "--tls-skip-verify",
            "--tls-ca",
            "ca.pem",
            "--tls-cert",
            "c.pem",
            "--tls-key",
            "k.pem",
            "--no-persist",
        ]),
        &[],
    );
    assert_eq!(c["tls"]["skip_verify"], true);
    assert_eq!(c["tls"]["ca_file"], "ca.pem");
    assert_eq!(c["persist"], false);
}

#[test]
fn connector_count_is_the_max_of_connectors_and_connector() {
    let c = config(&with_url(&["--connectors", "1", "-c", "3"]), &[]);
    assert_eq!(c["connector_count"], 3);
    assert_eq!(c["initial_connector"], 3);
    let c = config(&with_url(&["--connectors", "4", "-c", "2"]), &[]);
    assert_eq!(c["connector_count"], 4);
}

#[test]
fn invalid_values_are_rejected() {
    for extra in [
        vec!["--connectors", "0"],
        vec!["--heartbeat", "0"],
        vec!["--call-timeout-ms", "0"],
        vec!["--ws-ping", "-1"],
        vec!["--reconnect-initial-ms", "abc"],
        vec!["--id", ""],
    ] {
        let output = run(&with_url(&extra), &[]);
        assert!(!output.status.success(), "{extra:?} should fail");
    }
}

#[test]
fn bad_urls_are_rejected() {
    for url in ["http://localhost:9000", "localhost:9000", "ws://"] {
        let output = run(&["--url", url], &[]);
        assert!(!output.status.success(), "{url}");
    }
}

#[test]
fn empty_url_env_var_is_not_a_valid_url() {
    let output = run(&["--print-config"], &[("OCPP_URL", "")]);
    assert!(!output.status.success());
    assert_eq!(output.stdout, Vec::<u8>::new());
}

#[test]
fn tls_cert_without_key_fails_validation() {
    let output = run(&with_url(&["--tls-cert", "c.pem"]), &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("tls.cert_file"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn zero_reconnect_delay_with_unlimited_attempts_is_a_usage_error() {
    let output = run(&with_url(&["--reconnect-initial-ms", "0"]), &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("busy loop"), "{}", stderr(&output));
    // Bounded attempts make a zero delay acceptable.
    let shown = config(
        &with_url(&[
            "--reconnect-initial-ms",
            "0",
            "--reconnect-max-attempts",
            "3",
        ]),
        &[],
    );
    assert_eq!(shown["reconnect"]["initial_delay_ms"], 0);
}

#[test]
fn serial_and_firmware_flags_reach_the_config() {
    let mut args = URL.to_vec();
    args.extend(["--serial", "SN-9", "--firmware-version", "2.0.1"]);
    let c = config(&args, &[]);
    assert_eq!(c["serial_number"], "SN-9");
    assert_eq!(c["firmware_version"], "2.0.1");
    let none = config(URL, &[]);
    assert!(none["serial_number"].is_null());
}

#[test]
fn licenses_prints_notices_without_a_url() {
    let output = run(&["--licenses"], &[]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("MIT licence"), "{text}");
    assert!(text.contains("Open Charge Alliance"), "{text}");
    assert!(text.contains("Rust dependencies"), "{text}");
}

#[test]
fn help_lists_examples_and_exit_codes() {
    let output = run(&["--help"], &[]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Examples:"), "{text}");
    assert!(text.contains("--licenses"), "{text}");
}

#[test]
fn print_config_does_not_leak_url_credentials() {
    let output = run(
        &["--print-config", "--url", "ws://user:hunter2@h:9000/ocpp"],
        &[],
    );
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        !text.contains("hunter2") && !text.contains("user:"),
        "{text}"
    );
    assert!(text.contains("ws://h:9000/ocpp"), "{text}");
}

#[test]
fn rules_default_to_none() {
    assert_eq!(config(URL, &[])["response_rules"], serde_json::json!([]));
}

#[test]
fn rule_flag_is_repeatable_and_shown_in_print_config() {
    let c = config(
        &with_url(&[
            "--rule",
            "reset=reject",
            "--rule",
            "GetConfiguration=delay:5000*2",
            "--rule",
            "ChangeConfiguration=error:InternalError:boom",
        ]),
        &[],
    );
    let rules = c["response_rules"].as_array().unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0]["action"], "Reset");
    assert_eq!(rules[0]["behavior"]["kind"], "reject");
    assert_eq!(rules[0]["remaining"], Value::Null);
    assert_eq!(rules[1]["behavior"]["ms"], 5000);
    assert_eq!(rules[1]["remaining"], 2);
    assert_eq!(rules[2]["behavior"]["code"], "InternalError");
    assert_eq!(rules[2]["behavior"]["description"], "boom");
}

#[test]
fn rule_env_var_is_split_on_semicolons_and_the_flag_wins() {
    let args = with_url(&[]);
    let c = config(
        &args,
        &[("KIRBY_OCPP_RULES", "Reset=drop;ClearCache=ignore*1")],
    );
    assert_eq!(c["response_rules"].as_array().unwrap().len(), 2);
    let c = config(
        &with_url(&["--rule", "Reset=reject"]),
        &[("KIRBY_OCPP_RULES", "Reset=drop;ClearCache=ignore")],
    );
    let rules = c["response_rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1, "CLI > env");
    assert_eq!(rules[0]["behavior"]["kind"], "reject");
}

#[test]
fn invalid_rules_are_usage_errors() {
    for bad in [
        "Reset",
        "Heartbeat=reject",
        "Reset=explode",
        "Reset=delay:abc",
        "Reset=error:Nope",
        "Reset=reject*0",
    ] {
        let output = run(&with_url(&["--print-config", "--rule", bad]), &[]);
        assert_eq!(output.status.code(), Some(2), "{bad}");
        assert!(
            stderr(&output).contains("--rule"),
            "{bad}: {}",
            stderr(&output)
        );
    }
    let output = run(
        &with_url(&["--print-config"]),
        &[("KIRBY_OCPP_RULES", "Reset=explode")],
    );
    assert_eq!(output.status.code(), Some(2));
}
