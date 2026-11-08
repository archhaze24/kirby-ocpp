//! The conformance family end to end: the emulator scenarios in `conformance/scenarios` run, one
//! station each, against the real `kirby-csms` binary driven by `conformance/csms-script.toml`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kirby_ocpp_core::scenario::{CancelToken, Scenario, run_scenario_subscribed};
use kirby_ocpp_core::station::Snapshot;
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(60);

/// Station id the script and the scenarios agree on (`vars.station` of the script).
const STATION_ID: &str = "CP-CONF";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

/// A running `kirby-csms` that is killed when dropped.
struct Csms {
    child: Child,
    stderr: Arc<Mutex<String>>,
}

impl Drop for Csms {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pump_stderr(child: &mut Child) -> Arc<Mutex<String>> {
    let sink = Arc::new(Mutex::new(String::new()));
    let copy = Arc::clone(&sink);
    let pipe = child.stderr.take().expect("stderr is piped");
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            if let Ok(mut text) = copy.lock() {
                text.push_str(&line);
                text.push('\n');
            }
        }
    });
    sink
}

/// Starts the CSMS with the script; returns it and the address it listens on. Its stdout is
/// drained on a thread so a full pipe never stalls it.
fn start_csms(script: &Path, report: &Path) -> (Csms, String, mpsc::Receiver<String>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kirby-csms"));
    for (key, _) in std::env::vars() {
        if key.starts_with("KIRBY_CSMS_") {
            command.env_remove(key);
        }
    }
    command
        .args(["--listen", "127.0.0.1:0", "--json", "--no-tui", "--script"])
        .arg(script)
        .args(["--timeout", "120", "--report"])
        .arg(report)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("kirby-csms starts");
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = pump_stderr(&mut child);
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + WAIT;
    let addr = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left) {
            Ok(line) => {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && value["type"] == "listening"
                {
                    break value["addr"].as_str().expect("addr").to_owned();
                }
            }
            Err(RecvTimeoutError::Timeout) => panic!("kirby-csms did not report its address"),
            Err(RecvTimeoutError::Disconnected) => {
                panic!("kirby-csms exited early: {}", stderr.lock().unwrap());
            }
        }
    };
    (Csms { child, stderr }, addr, lines)
}

/// The scenarios of the family in the order of their file names.
fn family() -> Vec<Scenario> {
    let dir = root().join("scenarios");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("conformance/scenarios exists")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|path| Scenario::load(path).expect("scenario loads"))
        .collect()
}

#[test]
fn every_scenario_of_the_family_is_tagged_and_named_after_the_family() {
    let scenarios = family();
    assert!(scenarios.len() >= 15, "{}", scenarios.len());
    let mut names = std::collections::BTreeSet::new();
    for scenario in &scenarios {
        assert!(
            scenario.tags.iter().any(|t| t == "conformance"),
            "{} lacks the `conformance` tag",
            scenario.name
        );
        assert!(
            scenario.name.starts_with("conformance-"),
            "{}",
            scenario.name
        );
        assert!(
            names.insert(scenario.name.clone()),
            "{} twice",
            scenario.name
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_family_passes_against_the_scripted_csms() {
    let dir = tempfile::tempdir().unwrap();
    let report_path = dir.path().join("script-report.json");
    let (mut csms, addr, _lines) = start_csms(&root().join("csms-script.toml"), &report_path);

    let overall = Instant::now();
    for scenario in family() {
        let mut config =
            StationConfig::new(format!("ws://{addr}/ocpp")).with_charge_point_id(STATION_ID);
        config.persist = false;
        let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
        // Subscribed before connecting, as `kirby-ocpp suite` does: the scenario then sees the
        // CALLs that arrive right after the boot.
        let events = handle.subscribe();
        handle.connect().await.unwrap();
        let mut snapshots = handle.watch_snapshot();
        tokio::time::timeout(WAIT, snapshots.wait_for(Snapshot::booted))
            .await
            .unwrap_or_else(|_| panic!("{}: the station did not boot", scenario.name))
            .unwrap();
        let cancel = CancelToken::new();
        let report = tokio::time::timeout(
            WAIT,
            run_scenario_subscribed(
                &handle,
                &scenario,
                &cancel,
                None,
                &kirby_ocpp_core::scenario::Vars::default(),
                1,
                events,
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{}: timed out", scenario.name));
        assert!(
            report.passed,
            "{} failed: {report:#?}\ncsms stderr: {}",
            scenario.name,
            csms.stderr.lock().unwrap()
        );
        let _ = handle.shutdown().await;
        let _ = tokio::time::timeout(Duration::from_secs(5), join).await;
        assert!(
            overall.elapsed() < Duration::from_secs(110),
            "the family is too slow"
        );
    }

    // The script checked every answer on the CSMS side; its verdict is the exit code.
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = csms.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the script did not finish");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap())
        .expect("script report is JSON");
    assert!(
        status.success() && report["status"] == "passed",
        "script failed: {report:#}\nstderr: {}",
        csms.stderr.lock().unwrap()
    );
}
