//! OCPP 2.1 transaction resumption after process restart (E17) and remote reset (B13).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Duration, Utc};
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::messages::RegistrationStatus;
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, IdTokenType};
use kirby_ocpp_core::station2::TransactionLimit2;
use kirby_ocpp_core::{Clock, Command2, ManualClock, Station2, Station2Handle};
use kirby_ocpp_testkit::{MockCsms, Reply};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::station2_persistence::{config, loaded2, store};
use crate::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};
use crate::station2_transactions::{call, set_variables, stop};
use crate::station21::v21_csms;

const TIMEOUT: &str = "TxResumptionTimeout";
const ALLOW_ENERGY: &str = "TxAllowEnergyTransferResumption";

async fn boot(
    csms: &MockCsms,
    protocol: Protocol,
    dir: &Path,
    clock: &ManualClock,
) -> (Station2Handle, JoinHandle<()>) {
    let mut config = config(csms, dir).with_heartbeat_interval_secs(3600);
    config.protocol = protocol;
    protocol.token().clone_into(&mut config.ws_subprotocol);
    let (station, task) = Station2::spawn(config, Arc::new(clock.clone()));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    (station, task)
}

async fn settings(csms: &MockCsms, resume: bool, allow_energy: bool) {
    if resume {
        set_variables(csms, &[("TxCtrlr", TIMEOUT, None, "60")]).await;
    }
    if !allow_energy {
        set_variables(csms, &[("TxCtrlr", ALLOW_ENERGY, None, "false")]).await;
    }
}

async fn tx_event(csms: &MockCsms, mark: usize, trigger: &str) -> Value {
    csms.wait_for_call_after(
        mark,
        "TransactionEvent",
        |payload| payload["triggerReason"] == trigger,
        STEP,
    )
    .await
    .unwrap_or_else(|error| panic!("{trigger}: {error}"))
}

async fn start_tx(csms: &MockCsms, station: &Station2Handle, plug_in: bool) {
    if plug_in {
        station
            .send(Command2::PlugIn { evse: 1 })
            .await
            .expect("plug in");
    }
    station
        .send(Command2::PresentToken {
            evse: Some(1),
            id_token: "TAG-1".to_owned(),
            token_type: IdTokenType::Local,
        })
        .await
        .expect("present token");
    if plug_in {
        tx_event(csms, 0, "ChargingStateChanged").await;
    } else {
        tx_event(csms, 0, "Authorized").await;
    }
}

fn assert_updated(event: &Value, state: &str) {
    assert_eq!(event["eventType"], "Updated");
    assert_eq!(event["transactionInfo"]["chargingState"], state);
}

fn assert_no_ended(csms: &MockCsms) {
    assert!(
        csms.received()
            .iter()
            .filter(|call| call.action == "TransactionEvent")
            .all(|call| call.payload["eventType"] != "Ended")
    );
}

async fn assert_energy(csms: &MockCsms, station: &Station2Handle, mark: usize, baseline: u64) {
    tx_event(csms, mark, "MeterValuePeriodic").await;
    wait_snapshot(station, "the meter register to increase", |snapshot| {
        snapshot
            .evse(1)
            .is_some_and(|evse| evse.meter_wh > baseline)
    })
    .await;
}

#[tokio::test]
async fn resume_after_restart_keeps_sequence_connector_and_metering() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, true).await;
    station
        .send(Command2::AdvanceMeter {
            evse: 1,
            wh_delta: 1000,
        })
        .await
        .expect("nonzero start register");
    start_tx(&csms, &station, true).await;
    station
        .set_transaction_limit(
            1,
            TransactionLimit2 {
                max_time_s: Some(300),
                ..TransactionLimit2::default()
            },
        )
        .await
        .expect("set transaction limit");
    set_variables(
        &csms,
        &[("SampledDataCtrlr", "TxUpdatedInterval", None, "1")],
    )
    .await;
    clock.advance(Duration::seconds(5));
    stop(&station, task).await;
    let saved = loaded2(store(dir.path()).load2().expect("load saved state"));
    assert_eq!(saved.saved_at, Some(clock.now_utc()));
    let tx = &saved.tx.active[0];

    let mark = csms.mark();
    clock.advance(Duration::seconds(10));
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    let resumed = tx_event(&csms, mark, "TxResumed").await;
    assert_updated(&resumed, "Charging");
    assert_eq!(resumed["seqNo"], tx.next_seq_no);
    assert_eq!(resumed["transactionInfo"]["transactionId"], tx.id.0);
    csms.wait_for_call_after(
        mark,
        "StatusNotification",
        |payload| payload["evseId"] == 1 && payload["connectorStatus"] == "Occupied",
        STEP,
    )
    .await
    .expect("plugged connector reported after boot");
    let baseline = station.snapshot().evse(1).expect("EVSE 1").meter_wh;
    assert_energy(&csms, &station, mark, baseline).await;
    let subsequent = csms.calls_since(mark);
    assert_eq!(subsequent[0].action, "BootNotification");
    assert_eq!(
        subsequent
            .iter()
            .filter(|call| call.payload["triggerReason"] == "TxResumed")
            .count(),
        1
    );
    assert_no_ended(&csms);
    let mark = csms.mark();
    clock.advance(Duration::seconds(300));
    assert_updated(
        &tx_event(&csms, mark, "TimeLimitReached").await,
        "SuspendedEVSE",
    );
    stop(&station, task).await;
}

async fn assert_power_loss(protocol: Protocol, enable: bool, downtime: i64, missing_time: bool) {
    let csms = if protocol == Protocol::V21 {
        v21_csms().await
    } else {
        v201_csms().await
    };
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, protocol, dir.path(), &clock).await;
    if protocol == Protocol::V21 {
        settings(&csms, enable, true).await;
    }
    start_tx(&csms, &station, true).await;
    stop(&station, task).await;
    if missing_time || protocol == Protocol::V201 {
        let store = store(dir.path());
        let saved = loaded2(store.load2().expect("load saved state"));
        let mut value = serde_json::to_value(saved).expect("serialize state");
        if missing_time {
            value
                .as_object_mut()
                .expect("state object")
                .remove("saved_at");
        } else {
            value["device_model"]["values"]["TxCtrlr.TxResumptionTimeout"] = json!("60");
        }
        std::fs::write(store.path(), value.to_string()).expect("write state fixture");
    }
    let mark = csms.mark();
    clock.advance(Duration::seconds(downtime));
    let (station, task) = boot(&csms, protocol, dir.path(), &clock).await;
    let ended = tx_event(&csms, mark, "AbnormalCondition").await;
    assert_eq!(ended["eventType"], "Ended");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "PowerLoss");
    assert!(
        station
            .snapshot()
            .evse(1)
            .expect("EVSE 1")
            .transaction
            .is_none()
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn resume_tx_limit_preserves_a_fractional_start_register() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, true).await;
    start_tx(&csms, &station, true).await;
    stop(&station, task).await;

    let store = store(dir.path());
    let mut saved = loaded2(store.load2().expect("load saved state"));
    let tx = &mut saved.tx.active[0];
    tx.meter_start_wh = 100;
    tx.energy_wh = 9.5;
    tx.limit.max_energy_wh = Some(10.0);
    saved.tx.meters.insert(1, 110.25);
    store
        .save2(&saved)
        .expect("save fractional register fixture");

    let mark = csms.mark();
    clock.advance(Duration::seconds(1));
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    assert_updated(&tx_event(&csms, mark, "TxResumed").await, "Charging");
    assert_updated(
        &tx_event(&csms, mark, "EnergyLimitReached").await,
        "SuspendedEVSE",
    );
    stop(&station, task).await;
    let saved = loaded2(store.load2().expect("load capped register"));
    assert!((saved.tx.meters[&1] - 110.75).abs() < 1e-10);
    assert_eq!(saved.tx.active[0].energy_wh.to_bits(), 10.0_f64.to_bits());
}

#[tokio::test]
async fn resume_after_restart_expires() {
    assert_power_loss(Protocol::V21, true, 120, false).await;
}

#[tokio::test]
async fn resume_after_restart_is_disabled_by_default_and_on_2_0_1() {
    assert_power_loss(Protocol::V21, false, 10, false).await;
    assert_power_loss(Protocol::V201, true, 10, false).await;
}

#[tokio::test]
async fn resume_does_not_repeat_the_remote_start_id() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, true).await;
    station
        .send(Command2::PlugIn { evse: 1 })
        .await
        .expect("plug in");
    let reply = call(
        &csms,
        "RequestStartTransaction",
        json!({
            "evseId": 1,
            "idToken": {"idToken": "REMOTE", "type": "Central"},
            "remoteStartId": 42,
        }),
    )
    .await;
    assert_eq!(reply["status"], "Accepted");
    let started = tx_event(&csms, 0, "RemoteStart").await;
    assert_eq!(started["transactionInfo"]["remoteStartId"], 42);
    wait_snapshot(&station, "the remote start events to be delivered", |s| {
        s.queued_events == 0
            && s.evse(1)
                .and_then(|evse| evse.transaction.as_ref())
                .is_some_and(|tx| tx.charging_state == ChargingState::Charging)
    })
    .await;
    stop(&station, task).await;

    let mark = csms.mark();
    clock.advance(Duration::seconds(10));
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    assert_updated(&tx_event(&csms, mark, "TxResumed").await, "Charging");
    assert_eq!(
        csms.received()
            .iter()
            .filter(|call| {
                call.action == "TransactionEvent"
                    && call.payload["transactionInfo"]["remoteStartId"] == 42
            })
            .count(),
        1
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn resume_keeps_the_original_deferred_begin_sample_for_plug_in() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, true).await;
    set_variables(
        &csms,
        &[
            ("TxCtrlr", "TxStartPoint", None, "Authorized"),
            (
                "SampledDataCtrlr",
                "TxStartedMeasurands",
                None,
                "Energy.Active.Import.Register",
            ),
        ],
    )
    .await;
    station
        .send(Command2::AdvanceMeter {
            evse: 1,
            wh_delta: 100,
        })
        .await
        .expect("seed meter");
    start_tx(&csms, &station, false).await;
    let started = tx_event(&csms, 0, "Authorized").await;
    assert!(started.get("evse").is_none());
    assert!(started.get("meterValue").is_none());
    stop(&station, task).await;

    let mark = csms.mark();
    clock.advance(Duration::seconds(10));
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    let resumed = tx_event(&csms, mark, "TxResumed").await;
    assert_updated(&resumed, "Idle");
    assert!(resumed.get("meterValue").is_none());
    station
        .send(Command2::AdvanceMeter {
            evse: 1,
            wh_delta: 25,
        })
        .await
        .expect("change meter after restart");
    station
        .send(Command2::PlugIn { evse: 1 })
        .await
        .expect("plug in");
    let plugged = tx_event(&csms, mark, "CablePluggedIn").await;
    assert_eq!(plugged["evse"]["id"], 1);
    let meter = &plugged["meterValue"][0];
    assert_eq!(meter["timestamp"], started["timestamp"]);
    assert_eq!(meter["sampledValue"][0]["context"], "Transaction.Begin");
    assert_eq!(meter["sampledValue"][0]["value"].as_f64(), Some(100.0));
    stop(&station, task).await;
}

#[tokio::test]
async fn resume_after_restart_needs_a_known_nonnegative_interruption() {
    assert_power_loss(Protocol::V21, true, 10, true).await;
    assert_power_loss(Protocol::V21, true, -1, false).await;
}

#[tokio::test]
async fn resume_after_restart_can_inhibit_energy_transfer() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, false).await;
    start_tx(&csms, &station, true).await;
    stop(&station, task).await;

    let mark = csms.mark();
    clock.advance(Duration::seconds(60));
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    assert_updated(&tx_event(&csms, mark, "TxResumed").await, "SuspendedEVSE");
    assert_eq!(
        station.snapshot().evse(1).expect("EVSE 1").offered_w,
        Some(0.0)
    );
    assert_no_ended(&csms);
    stop(&station, task).await;
}

#[tokio::test]
async fn resume_does_not_transfer_energy_on_an_inoperative_connector() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, true).await;
    start_tx(&csms, &station, true).await;
    stop(&station, task).await;

    let store = store(dir.path());
    let mut saved = loaded2(store.load2().expect("load saved state"));
    assert_eq!(saved.tx.active[0].charging_state, ChargingState::Charging);
    let evse = saved
        .evse_availability
        .iter_mut()
        .find(|evse| evse.evse == 1)
        .expect("saved EVSE 1");
    evse.connectors = vec![(1, false)];
    store
        .save2(&saved)
        .expect("save inoperative connector fixture");

    let mark = csms.mark();
    clock.advance(Duration::seconds(10));
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    assert_updated(&tx_event(&csms, mark, "TxResumed").await, "SuspendedEVSE");
    csms.wait_for_call_after(
        mark,
        "StatusNotification",
        |event| event["evseId"] == 1 && event["connectorStatus"] == "Unavailable",
        STEP,
    )
    .await
    .expect("the restored connector is unavailable");
    let baseline = station.snapshot().evse(1).unwrap().meter_wh;
    set_variables(
        &csms,
        &[("SampledDataCtrlr", "TxUpdatedInterval", None, "1")],
    )
    .await;
    tx_event(&csms, mark, "MeterValuePeriodic").await;
    let snapshot = station.snapshot();
    let evse = snapshot.evse(1).unwrap();
    assert_eq!(evse.offered_w, Some(0.0));
    assert_eq!(evse.meter_wh, baseline);
    stop(&station, task).await;
}

#[tokio::test]
async fn resume_after_restart_without_an_ev_is_idle() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, true).await;
    set_variables(&csms, &[("TxCtrlr", "TxStartPoint", None, "Authorized")]).await;
    start_tx(&csms, &station, false).await;
    stop(&station, task).await;

    let mark = csms.mark();
    clock.advance(Duration::seconds(10));
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    assert_updated(&tx_event(&csms, mark, "TxResumed").await, "Idle");
    assert_no_ended(&csms);
    stop(&station, task).await;
}

async fn reset(csms: &MockCsms) {
    assert_eq!(
        call(csms, "Reset", json!({"type": "ImmediateAndResume"})).await["status"],
        "Accepted"
    );
}

fn hold_reset_boot(csms: &MockCsms, clock: &ManualClock) -> Arc<AtomicBool> {
    let boot_clock = clock.clone();
    let accept_boot = Arc::new(AtomicBool::new(false));
    let can_accept = Arc::clone(&accept_boot);
    csms.respond("BootNotification", move |_| {
        let accept = can_accept.load(Ordering::Relaxed);
        Reply::Result(json!({
            "status": if accept { "Accepted" } else { "Pending" },
            "currentTime": boot_clock.now_utc().to_rfc3339(),
            "interval": if accept { 3600 } else { 1 },
        }))
    });
    accept_boot
}

async fn assert_reset_resume(allow_energy: bool) {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, allow_energy).await;
    start_tx(&csms, &station, true).await;
    set_variables(&csms, &[("TxCtrlr", "TxStopPoint", None, "EnergyTransfer")]).await;
    let accept_boot = hold_reset_boot(&csms, &clock);
    let mark = csms.mark();
    reset(&csms).await;
    let suspended = tx_event(&csms, mark, "ResetCommand").await;
    assert_updated(&suspended, "SuspendedEVSE");
    let pending = wait_snapshot(&station, "the reboot's boot to be pending", |snapshot| {
        snapshot.registration == Some(RegistrationStatus::Pending)
    })
    .await;
    let tx = pending
        .evse(1)
        .unwrap()
        .transaction
        .as_ref()
        .expect("ongoing transaction");
    assert_eq!(tx.charging_state, ChargingState::SuspendedEvse);
    assert_eq!(
        Some(i64::from(tx.next_seq_no)),
        suspended["seqNo"].as_i64().map(|n| n + 1)
    );
    // B13 resumes even if the reset takes longer than the resumption timeout.
    clock.advance(Duration::seconds(120));
    accept_boot.store(true, Ordering::Relaxed);
    let resumed = tx_event(&csms, mark, "TxResumed").await;
    assert_updated(
        &resumed,
        if allow_energy {
            "Charging"
        } else {
            "SuspendedEVSE"
        },
    );
    assert_eq!(
        resumed["seqNo"].as_i64(),
        suspended["seqNo"].as_i64().map(|n| n + 1)
    );
    assert_eq!(
        resumed["transactionInfo"]["transactionId"],
        suspended["transactionInfo"]["transactionId"]
    );
    let calls: Vec<_> = csms
        .calls_since(mark)
        .into_iter()
        .filter(|call| {
            matches!(
                call.action.as_str(),
                "TransactionEvent" | "BootNotification"
            )
        })
        .collect();
    assert!(calls.len() >= 4);
    assert_eq!(calls[0].payload["triggerReason"], "ResetCommand");
    let resumed_call = calls.last().expect("resume CALL");
    assert_eq!(resumed_call.payload["triggerReason"], "TxResumed");
    for boot in &calls[1..calls.len() - 1] {
        assert_eq!(boot.action, "BootNotification");
        assert_eq!(boot.payload["reason"], "RemoteReset");
        assert_eq!(boot.connection, resumed_call.connection);
        assert_ne!(boot.connection, calls[0].connection);
    }
    csms.wait_for_call_after(
        mark,
        "SecurityEventNotification",
        |event| event["type"] == "ResetOrReboot",
        STEP,
    )
    .await
    .expect("reset security event");
    let snapshot = station.snapshot();
    let evse = snapshot.evse(1).expect("EVSE 1");
    if allow_energy {
        let mark = csms.mark();
        set_variables(
            &csms,
            &[("SampledDataCtrlr", "TxUpdatedInterval", None, "1")],
        )
        .await;
        assert_energy(&csms, &station, mark, evse.meter_wh).await;
    } else {
        assert_eq!(evse.offered_w, Some(0.0));
    }
    assert_no_ended(&csms);
    stop(&station, task).await;
}

#[tokio::test]
async fn reset_resume_suspends_then_resumes_after_accepted_boot() {
    assert_reset_resume(true).await;
}

#[tokio::test]
async fn reset_resume_can_keep_energy_transfer_suspended() {
    assert_reset_resume(false).await;
}

#[tokio::test]
async fn reset_resume_keeps_energy_suspended_after_cost_limit_is_reached() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, true).await;
    csms.respond("TransactionEvent", |event| {
        Reply::Result(if event["triggerReason"] == "ResetCommand" {
            json!({"totalCost": 5})
        } else {
            json!({})
        })
    });
    start_tx(&csms, &station, true).await;
    station
        .set_transaction_limit(
            1,
            TransactionLimit2 {
                max_cost: Some(5.0),
                ..TransactionLimit2::default()
            },
        )
        .await
        .expect("set cost limit");
    let mark = csms.mark();
    reset(&csms).await;
    let resumed = tx_event(&csms, mark, "TxResumed").await;
    assert_updated(&resumed, "SuspendedEVSE");
    let baseline = station.snapshot().evse(1).unwrap().meter_wh;
    set_variables(
        &csms,
        &[("SampledDataCtrlr", "TxUpdatedInterval", None, "1")],
    )
    .await;
    tx_event(&csms, mark, "MeterValuePeriodic").await;
    let snapshot = station.snapshot();
    let evse = snapshot.evse(1).unwrap();
    assert_eq!(evse.limit_reached.as_deref(), Some("CostLimitReached"));
    assert_eq!(evse.offered_w, Some(0.0));
    assert_eq!(evse.meter_wh, baseline);
    stop(&station, task).await;
}

#[tokio::test]
async fn reset_resume_defers_energy_when_limit_is_raised_during_pending_boot() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, true).await;
    start_tx(&csms, &station, true).await;
    let set_energy_limit = async |max| {
        let outcome = tokio::time::timeout(
            STEP,
            station.set_transaction_limit(
                1,
                TransactionLimit2 {
                    max_energy_wh: Some(max),
                    ..TransactionLimit2::default()
                },
            ),
        )
        .await
        .expect("limit command deadline")
        .expect("set energy limit");
        assert_eq!(outcome, kirby_ocpp_core::Outcome2::Done);
    };
    set_energy_limit(10.0).await;
    station
        .send(Command2::AdvanceMeter {
            evse: 1,
            wh_delta: 10,
        })
        .await
        .expect("advance energy to the limit");
    assert_updated(
        &tx_event(&csms, 0, "EnergyLimitReached").await,
        "SuspendedEVSE",
    );
    let accept_boot = hold_reset_boot(&csms, &clock);
    let mark = csms.mark();
    reset(&csms).await;
    let pending = wait_snapshot(&station, "the reset boot to be pending", |snapshot| {
        snapshot.registration == Some(RegistrationStatus::Pending)
    })
    .await;
    let baseline = pending.evse(1).unwrap().meter_wh;
    set_energy_limit(20.0).await;
    let pending = wait_snapshot(&station, "the raised limit", |snapshot| {
        snapshot
            .evse(1)
            .and_then(|evse| evse.transaction_limit?.max_energy_wh)
            == Some(20.0)
    })
    .await;
    let evse = pending.evse(1).unwrap();
    assert_eq!(evse.limit_reached, None);
    assert_eq!(
        evse.transaction.as_ref().unwrap().charging_state,
        ChargingState::SuspendedEvse
    );
    assert_eq!(evse.offered_w, Some(0.0));
    assert_eq!(evse.meter_wh, baseline);

    accept_boot.store(true, Ordering::Relaxed);
    let limit_set = tx_event(&csms, mark, "LimitSet").await;
    assert!(limit_set["transactionInfo"].get("chargingState").is_none());
    assert_updated(&tx_event(&csms, mark, "TxResumed").await, "Charging");
    set_variables(
        &csms,
        &[("SampledDataCtrlr", "TxUpdatedInterval", None, "1")],
    )
    .await;
    assert_energy(&csms, &station, mark, baseline).await;
    stop(&station, task).await;
}

#[tokio::test]
async fn reset_resume_without_transactions_reboots_and_rejects_evse_reset() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), &clock).await;
    settings(&csms, true, true).await;
    let reply = call(
        &csms,
        "Reset",
        json!({"type": "ImmediateAndResume", "evseId": 1}),
    )
    .await;
    assert_eq!(reply["status"], "Rejected");
    assert_eq!(reply["statusInfo"]["reasonCode"], "UnsupportedParam");
    assert_eq!(csms.connections(), 1);
    let mark = csms.mark();
    reset(&csms).await;
    let boot = csms
        .wait_for_call_after(mark, "BootNotification", |_| true, STEP)
        .await
        .expect("reset boot");
    assert_eq!(boot["reason"], "RemoteReset");
    assert_eq!(csms.connections(), 2);
    assert!(
        csms.calls_since(mark)
            .iter()
            .all(|call| call.action != "TransactionEvent")
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn resume_variables_exist_only_on_2_1() {
    let dir = tempfile::tempdir().expect("state directory");
    for protocol in [Protocol::V21, Protocol::V201] {
        let csms = if protocol == Protocol::V21 {
            v21_csms().await
        } else {
            v201_csms().await
        };
        let (station, task) =
            boot(&csms, protocol, dir.path(), &ManualClock::new(Utc::now())).await;
        let query = [TIMEOUT, ALLOW_ENERGY].map(|name| {
            json!({
                "component": {"name": "TxCtrlr"}, "variable": {"name": name},
            })
        });
        let result = call(&csms, "GetVariables", json!({"getVariableData": query})).await;
        let rows = result["getVariableResult"]
            .as_array()
            .expect("variable results");
        assert_eq!(rows.len(), 2);
        for (row, default) in rows.iter().zip(["0", "true"]) {
            if protocol == Protocol::V21 {
                assert_eq!(row["attributeStatus"], "Accepted");
                assert_eq!(row["attributeValue"], default);
            } else {
                assert_eq!(row["attributeStatus"], "UnknownVariable");
            }
        }
        stop(&station, task).await;
    }
}
