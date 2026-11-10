//! The conformance family end to end: the emulator scenarios in `conformance/scenarios` run, one
//! station each, against the real `kirby-csms` binary driven by `conformance/csms-script.toml`.
//!
//! Every wait has its own deadline and a message that names what stalled. A scenario is not
//! started before the CSMS has seen the previous station go away, and the CSMS process is
//! watched while the test waits: if it exits early (a failed script ends it), the test says so
//! and prints the script report instead of waiting for a boot that can never come.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kirby_ocpp_core::scenario::{
    CancelToken, Scenario, ScenarioProgress, Vars, run_scenario_subscribed,
};
use kirby_ocpp_core::station::Snapshot;
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use serde_json::Value;
use tokio::sync::mpsc::unbounded_channel;

/// Longest a station may take to connect and boot (a scenario takes about a second).
const BOOT_WAIT: Duration = Duration::from_secs(30);
/// Longest one scenario may run (the slowest takes about two seconds).
const RUN_WAIT: Duration = Duration::from_secs(45);
/// Longest the CSMS may take to notice that a station is gone, and the script to finish.
const TEARDOWN_WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(25);

/// Station id the script and the scenarios agree on (`vars.station` of the script).
const STATION_ID: &str = "CP-CONF";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

/// A running `kirby-csms` that is killed when dropped, with everything it printed.
struct Csms {
    child: Child,
    output: Arc<Mutex<Vec<String>>>,
    report_path: PathBuf,
}

impl Drop for Csms {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pump<R: std::io::Read + Send + 'static>(
    pipe: R,
    prefix: &'static str,
    sink: &Arc<Mutex<Vec<String>>>,
) {
    let sink = Arc::clone(sink);
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            if let Ok(mut lines) = sink.lock() {
                lines.push(format!("{prefix}{line}"));
            }
        }
    });
}

impl Csms {
    /// Starts the CSMS with the script and waits for the address it listens on.
    fn start(script: &Path, report_path: &Path) -> (Self, String) {
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
            .arg(report_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("kirby-csms starts");
        let output = Arc::new(Mutex::new(Vec::new()));
        pump(child.stdout.take().expect("stdout"), "", &output);
        pump(child.stderr.take().expect("stderr"), "stderr: ", &output);
        let mut csms = Self {
            child,
            output,
            report_path: report_path.to_owned(),
        };
        let deadline = Instant::now() + BOOT_WAIT;
        let addr = loop {
            if let Some(addr) = csms.json_lines().into_iter().find_map(|v| {
                (v["type"] == "listening").then(|| v["addr"].as_str().map(str::to_owned))?
            }) {
                break addr;
            }
            csms.assert_running("starting");
            assert!(
                Instant::now() < deadline,
                "kirby-csms did not report its address\n{}",
                csms.dump()
            );
            thread::sleep(POLL);
        };
        (csms, addr)
    }

    /// The JSON event lines printed so far.
    fn json_lines(&self) -> Vec<Value> {
        let lines = self.output.lock().unwrap();
        lines
            .iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Number of `connected` and `disconnected` events of the test station so far.
    fn connection_events(&self) -> (usize, usize) {
        let events = self.json_lines();
        let count = |kind: &str| {
            events
                .iter()
                .filter(|v| v["type"] == kind && v["station"] == STATION_ID)
                .count()
        };
        (count("connected"), count("disconnected"))
    }

    /// The last lines the CSMS printed and its script report, for failure messages.
    fn dump(&self) -> String {
        let lines = self.output.lock().unwrap();
        let tail = lines
            .iter()
            .rev()
            .take(40)
            .rev()
            .map(|l| l.chars().take(300).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        let report = std::fs::read_to_string(&self.report_path).unwrap_or_default();
        format!("--- last CSMS output ---\n{tail}\n--- script report ---\n{report}")
    }

    fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().expect("try_wait")
    }

    /// Fails at once, with the output, when the CSMS has ended while `doing` needs it.
    fn assert_running(&mut self, doing: &str) {
        if let Some(status) = self.exited() {
            panic!(
                "kirby-csms exited ({status}) while {doing}; a failed script step ends it\n{}",
                self.dump()
            );
        }
    }
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

/// Runs one scenario on a fresh station and tears the station down completely.
async fn run_one(csms: &mut Csms, addr: &str, scenario: &Scenario, last: bool) {
    let name = &scenario.name;
    let (connected_before, disconnected_before) = csms.connection_events();
    assert_eq!(
        connected_before,
        disconnected_before,
        "{name}: the CSMS still shows a station connected before the scenario starts\n{}",
        csms.dump()
    );

    let mut config =
        StationConfig::new(format!("ws://{addr}/ocpp")).with_charge_point_id(STATION_ID);
    config.persist = false;
    let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
    // Subscribed before connecting, as `kirby-ocpp suite` does: the scenario then sees the
    // CALLs that arrive right after the boot.
    let events = handle.subscribe();
    handle.connect().await.unwrap();

    // Boot, watching the CSMS process so a dead script is reported as such.
    let mut snapshots = handle.watch_snapshot();
    let deadline = tokio::time::Instant::now() + BOOT_WAIT;
    loop {
        tokio::select! {
            booted = snapshots.wait_for(Snapshot::booted) => {
                booted.unwrap();
                break;
            }
            () = tokio::time::sleep(Duration::from_millis(200)) => {
                csms.assert_running(&format!("{name} waited for the station to boot"));
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "{name}: the station did not boot within {BOOT_WAIT:?}\n{}",
                    csms.dump()
                );
            }
        }
    }

    // Run, noting the step in flight so a stall names it.
    let cancel = CancelToken::new();
    let (progress, mut steps) = unbounded_channel();
    let vars = Vars::default();
    let run = run_scenario_subscribed(
        &handle,
        scenario,
        &cancel,
        Some(&progress),
        &vars,
        1,
        events,
    );
    tokio::pin!(run);
    let deadline = tokio::time::Instant::now() + RUN_WAIT;
    let mut step = String::from("(none yet)");
    let report = loop {
        tokio::select! {
            report = &mut run => break report,
            Some(ScenarioProgress::StepStarted { index, name }) = steps.recv() => {
                step = format!("step {} `{name}`", index + 1);
            }
            () = tokio::time::sleep(Duration::from_millis(200)) => {
                csms.assert_running(&format!("{name} ran {step}"));
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "{name}: stalled for {RUN_WAIT:?} at {step}\n{}",
                    csms.dump()
                );
            }
        }
    };
    assert!(report.passed, "{name} failed: {report:#?}\n{}", csms.dump());

    // Teardown: the station task ends, then the CSMS shows the connection closed, so the next
    // station never meets the previous one (the CSMS would treat it as a takeover).
    handle.shutdown().await.ok();
    tokio::time::timeout(TEARDOWN_WAIT, join)
        .await
        .unwrap_or_else(|_| panic!("{name}: the station task did not end"))
        .ok();
    let deadline = Instant::now() + TEARDOWN_WAIT;
    loop {
        let (connected, disconnected) = csms.connection_events();
        if connected == disconnected && connected > connected_before {
            break;
        }
        // The final script may exit successfully before its stdout reader delivers the
        // disconnect event. The station task has already ended; no next scenario needs it.
        if last {
            if let Some(status) = csms.exited() {
                assert!(
                    status.success(),
                    "{name}: CSMS exited ({status})\n{}",
                    csms.dump()
                );
                break;
            }
        } else {
            csms.assert_running(&format!("{name} waited for the station to disconnect"));
        }
        assert!(
            Instant::now() < deadline,
            "{name}: the CSMS never saw the station disconnect ({connected} connected, \
             {disconnected} disconnected)\n{}",
            csms.dump()
        );
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_family_passes_against_the_scripted_csms() {
    let dir = tempfile::tempdir().unwrap();
    let report_path = dir.path().join("script-report.json");
    let (mut csms, addr) = Csms::start(&root().join("csms-script.toml"), &report_path);

    let mut scenarios = family().into_iter().peekable();
    while let Some(scenario) = scenarios.next() {
        run_one(&mut csms, &addr, &scenario, scenarios.peek().is_none()).await;
    }

    // The script checked every answer on the CSMS side; its verdict is the exit code.
    let deadline = Instant::now() + TEARDOWN_WAIT;
    let status = loop {
        if let Some(status) = csms.exited() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the script did not finish\n{}",
            csms.dump()
        );
        tokio::time::sleep(POLL).await;
    };
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap())
        .expect("script report is JSON");
    assert!(
        status.success() && report["status"] == "passed",
        "script failed: {report:#}\n{}",
        csms.dump()
    );
}
