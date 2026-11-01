//! Shared helpers for the `station_*` integration tests.
//!
//! Every test builds its own [`Rig`]: a mock CSMS on an ephemeral port, a station with its own
//! temporary state directory and a [`ManualClock`]. Waits are event driven (snapshots, log
//! events, recorded CALLs) and bounded by [`STEP`]; nothing sleeps for a fixed time except the
//! few deliberate "nothing must happen" checks, which say so.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]
#![allow(
    clippy::match_wild_err_arm,
    reason = "a timeout is the only error these waits can produce"
)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_core::station::{Command, Event, FrameRecord, Snapshot, StationHandle};
use kirby_ocpp_core::{ManualClock, Station, StationConfig};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms};
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Upper bound for any single wait. Only reached when something is broken.
pub const STEP: Duration = Duration::from_secs(8);

pub fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap()
}

/// A station config for tests: short timeouts, no pings, fast reconnect, own state dir.
pub fn test_config(url: &str, state_dir: &TempDir) -> StationConfig {
    let mut config = StationConfig::new(url)
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_reconnect(ReconnectConfig {
            enabled: true,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .with_state_dir(state_dir.path());
    config.ws_ping_interval_secs = 0;
    config
}

/// What a test observed on the event stream.
pub struct Observed {
    rx: broadcast::Receiver<Event>,
    pub logs: Vec<String>,
    pub frames: Vec<FrameRecord>,
    log_cursor: usize,
}

impl Observed {
    pub fn new(rx: broadcast::Receiver<Event>) -> Self {
        Self {
            rx,
            logs: Vec::new(),
            frames: Vec::new(),
            log_cursor: 0,
        }
    }

    /// Moves everything that is already queued into `logs` / `frames`.
    pub fn drain(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.record(event);
        }
    }

    fn record(&mut self, event: Event) {
        match event {
            Event::Log(record) => self.logs.push(record.text),
            Event::Frame(record) => self.frames.push(record),
            Event::StateChanged => {}
        }
    }

    /// Receives events until `pred` holds for what has been recorded so far.
    pub async fn wait_until(&mut self, what: &str, pred: impl Fn(&Self) -> bool) {
        self.drain();
        while !pred(self) {
            match timeout(STEP, self.rx.recv()).await {
                Ok(Ok(event)) => self.record(event),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    panic!("event stream closed waiting for {what}")
                }
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    /// Waits for a log line containing `needle` that arrived after the previous match.
    pub async fn wait_log(&mut self, needle: &str) -> String {
        loop {
            if let Some(offset) = self.logs[self.log_cursor..]
                .iter()
                .position(|line| line.contains(needle))
            {
                let index = self.log_cursor + offset;
                self.log_cursor = index + 1;
                return self.logs[index].clone();
            }
            match timeout(STEP, self.rx.recv()).await {
                Ok(Ok(event)) => self.record(event),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    panic!(
                        "event stream closed waiting for log `{needle}`; saw {:#?}",
                        self.logs
                    )
                }
                Err(_) => panic!("timed out waiting for log `{needle}`; saw {:#?}", self.logs),
            }
        }
    }
}

pub struct Rig {
    pub csms: MockCsms,
    pub station: StationHandle,
    pub join: Option<JoinHandle<()>>,
    pub dir: TempDir,
    pub clock: ManualClock,
    pub observed: Observed,
}

impl Rig {
    pub async fn start() -> Self {
        Self::with(CsmsOptions::default(), |config| config).await
    }

    pub async fn with(
        options: CsmsOptions,
        tweak: impl FnOnce(StationConfig) -> StationConfig,
    ) -> Self {
        let csms = MockCsms::start(options).await.expect("mock CSMS starts");
        let dir = tempfile::tempdir().expect("temp dir");
        let clock = ManualClock::new(t0());
        let config = tweak(test_config(csms.url(), &dir));
        let (station, join) = Station::spawn(config, Arc::new(clock.clone()));
        let observed = Observed::new(station.subscribe());
        Self {
            csms,
            station,
            join: Some(join),
            dir,
            clock,
            observed,
        }
    }

    /// Connects and waits until the CSMS accepted the boot and the statuses were sent.
    pub async fn connect_booted(&mut self) {
        self.station.connect().await.expect("connect");
        self.wait_registration(RegistrationStatus::Accepted).await;
        // The forced status re-send follows the boot: connector 0 and every connector.
        let connectors = self.station.snapshot().connectors.len();
        self.csms
            .wait_for_call_count_after(0, "StatusNotification", connectors + 1, |_| true, STEP)
            .await
            .expect("initial StatusNotifications");
    }

    pub async fn wait_registration(&self, status: RegistrationStatus) -> Snapshot {
        wait_state(&self.station, &format!("registration {status}"), |s| {
            s.registration == Some(status)
        })
        .await
    }

    pub async fn shutdown(mut self) {
        self.station.shutdown().await.expect("shutdown");
        if let Some(join) = self.join.take() {
            join.await.expect("actor ends");
        }
        self.csms.shutdown().await;
    }

    /// The CALLs of `action` the CSMS has recorded since `mark`.
    pub fn calls(&self, action: &str, mark: usize) -> Vec<Value> {
        self.csms
            .calls_since(mark)
            .into_iter()
            .filter(|call| call.action == action)
            .map(|call| call.payload)
            .collect()
    }

    /// Asserts that the CSMS recorded no `action` CALL since `mark`.
    pub fn assert_no_calls(&self, action: &str, mark: usize) {
        assert_eq!(
            self.calls(action, mark),
            Vec::<Value>::new(),
            "unexpected {action} CALL(s)"
        );
    }

    /// Waits for a `StatusNotification` of `connector` with `status` made after `mark`.
    pub async fn wait_status(&self, mark: usize, connector: u32, status: &str) -> Value {
        self.csms
            .wait_for_call_after(
                mark,
                "StatusNotification",
                |p| p["connectorId"] == connector && p["status"] == status,
                STEP,
            )
            .await
            .unwrap_or_else(|e| panic!("StatusNotification({connector}, {status}): {e}"))
    }

    pub async fn send(&self, command: Command) -> kirby_ocpp_core::station::Outcome {
        self.station.send(command).await.expect("command succeeds")
    }
}

/// Waits until `pred` holds for the station's snapshot.
pub async fn wait_state(
    station: &StationHandle,
    what: &str,
    pred: impl Fn(&Snapshot) -> bool,
) -> Snapshot {
    let mut rx = station.watch_snapshot();
    match timeout(STEP, rx.wait_for(|s| pred(s))).await {
        Ok(Ok(snapshot)) => snapshot.clone(),
        Ok(Err(_)) => panic!("station stopped while waiting for {what}"),
        Err(_) => panic!(
            "timed out waiting for {what}; last snapshot: {:#?}",
            station.snapshot()
        ),
    }
}

/// An address nothing listens on (bound, then released).
pub async fn dead_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("ws://127.0.0.1:{port}/ocpp")
}
