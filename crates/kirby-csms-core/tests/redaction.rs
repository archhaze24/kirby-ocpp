//! `AuthorizationKey` values must not reach recorded frames (CSMS history and events, station
//! `Event::Frame`) unless `show_secrets` is set. The wire always carries the real value.

use std::sync::Arc;
use std::time::Duration;

use kirby_csms_core::{CsmsConfig, CsmsEvent, CsmsServer};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::types::RegistrationStatus;
use kirby_ocpp_core::station::{Direction, Event};
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use serde_json::json;
use tokio::time::timeout;

const STEP: Duration = Duration::from_secs(10);
const SECRET: &str = "0123456789abcdef0123";

/// Runs one `ChangeConfiguration(AuthorizationKey)` and returns
/// `(csms history frames, csms events, station events)` as JSON text.
async fn scenario(show_secrets: bool) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut csms_config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
    csms_config.show_secrets = show_secrets;
    let csms = CsmsServer::start(csms_config).await.unwrap();
    let mut csms_events = csms.subscribe();

    let dir = tempfile::tempdir().unwrap();
    let mut config = StationConfig::new(format!("{}/ocpp", csms.url()))
        .with_charge_point_id("CP-R")
        .with_call_timeout_ms(3_000)
        .with_reconnect(ReconnectConfig {
            enabled: false,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .with_state_dir(dir.path());
    config.ws_ping_interval_secs = 0;
    config.show_secrets = show_secrets;
    let (station, _join) = Station::spawn(config, Arc::new(SystemClock));
    let mut station_events = station.subscribe();
    station.connect().await.unwrap();
    let mut snapshots = station.watch_snapshot();
    timeout(
        STEP,
        snapshots.wait_for(|s| s.registration == Some(RegistrationStatus::Accepted)),
    )
    .await
    .unwrap()
    .unwrap();

    let reply = csms
        .call(
            "CP-R",
            "ChangeConfiguration",
            json!({"key": "AuthorizationKey", "value": SECRET}),
        )
        .await
        .unwrap();
    assert_eq!(reply.to_value()[2]["status"], "Accepted", "{reply:?}");

    // The reply frame is the last thing recorded on both sides.
    let mut station_frames = Vec::new();
    timeout(STEP, async {
        loop {
            if let Ok(Event::Frame(record)) = station_events.recv().await {
                let done = record.direction == Direction::Out
                    && record.action.as_deref() == Some("ChangeConfiguration");
                station_frames.push(record.frame.to_string());
                if done {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    let mut events = Vec::new();
    while let Ok(event) = csms_events.try_recv() {
        if let CsmsEvent::Frame { frame, .. } = event {
            events.push(frame.to_json_string());
        }
    }
    let history = csms
        .history("CP-R")
        .unwrap()
        .into_iter()
        .map(|entry| entry.frame.to_json_string())
        .collect();
    (history, events, station_frames)
}

fn only(frames: &[String]) -> Vec<&String> {
    frames
        .iter()
        .filter(|f| f.contains("ChangeConfiguration"))
        .collect()
}

#[tokio::test]
async fn recorded_frames_hide_the_authorization_key_by_default() {
    let (history, events, station) = scenario(false).await;
    for frames in [&history, &events, &station] {
        let calls = only(frames);
        assert_eq!(calls.len(), 1, "{frames:?}");
        assert!(calls[0].contains("\"***\""), "{}", calls[0]);
        assert!(frames.iter().all(|f| !f.contains(SECRET)), "{frames:?}");
    }
}

#[tokio::test]
async fn show_secrets_keeps_the_authorization_key() {
    let (history, events, station) = scenario(true).await;
    for frames in [&history, &events, &station] {
        let calls = only(frames);
        assert_eq!(calls.len(), 1, "{frames:?}");
        assert!(calls[0].contains(SECRET), "{}", calls[0]);
    }
}
