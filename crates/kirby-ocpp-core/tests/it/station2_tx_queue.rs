//! The outbound queue of transaction events: offline delivery, retries and ordering.
//!
//! The cap and the thinning of `Updated` events are unit tested next to the queue. Retry delays
//! are asserted by order and by the CSMS-side receive times (lower bounds only), never by an
//! upper bound on wall-clock time.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use kirby_ocpp_core::ocpp::v2::messages::RegistrationStatus;
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, Component, Variable};
use kirby_ocpp_core::{Clock, Command2, ManualClock};
use kirby_ocpp_testkit::{Fault, MockCsms, Reply};
use serde_json::{Value, json};

use crate::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};
use crate::station2_transactions::{
    booted, events, send, set_variables, spawn_with_clock, stop, wait_events, wait_queue_empty,
};

/// `TxStartPoint=EVConnected` and a one-second retry interval, so sessions need no token and
/// retries are quick.
async fn quick_cable_sessions(csms: &MockCsms, attempts: Option<&str>) {
    set_variables(
        csms,
        &[
            ("TxCtrlr", "TxStartPoint", None, "EVConnected"),
            (
                "OCPPCommCtrlr",
                "MessageAttemptInterval",
                Some("TransactionEvent"),
                "1",
            ),
        ],
    )
    .await;
    if let Some(attempts) = attempts {
        set_variables(
            csms,
            &[(
                "OCPPCommCtrlr",
                "MessageAttempts",
                Some("TransactionEvent"),
                attempts,
            )],
        )
        .await;
    }
}

fn seqs(events: &[Value]) -> Vec<i64> {
    events.iter().filter_map(|e| e["seqNo"].as_i64()).collect()
}

fn transaction_event_calls(csms: &MockCsms) -> Vec<kirby_ocpp_testkit::Received> {
    csms.calls_since(0)
        .into_iter()
        .filter(|call| call.action == "TransactionEvent")
        .collect()
}

fn boot_reply(status: &str) -> Value {
    json!({
        "currentTime": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "interval": 1,
        "status": status,
    })
}

#[tokio::test]
async fn events_generated_offline_arrive_in_order_with_their_original_content() {
    let csms = v201_csms().await;
    let clock = ManualClock::new(Utc::now());
    let generated_at = clock.now_utc();
    let (station, task) = spawn_with_clock(&csms, Arc::new(clock.clone()));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    set_variables(&csms, &[("TxCtrlr", "TxStartPoint", None, "EVConnected")]).await;

    // The whole session happens while the link is down.
    station.disconnect().await.expect("disconnect");
    send(&station, Command2::PlugIn { evse: 1 }).await;
    send(
        &station,
        Command2::SetChargingState {
            evse: 1,
            state: ChargingState::SuspendedEv,
        },
    )
    .await;
    send(&station, Command2::Unplug { evse: 1 }).await;
    wait_snapshot(&station, "three queued events", |s| s.queued_events == 3).await;
    assert!(events(&csms).is_empty(), "nothing is sent without a link");

    // Time passes; the events must not be regenerated.
    clock.advance(ChronoDuration::hours(1));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    let delivered = wait_events(&csms, 3).await;
    assert_eq!(seqs(&delivered), [0, 1, 2]);
    let kinds: Vec<&str> = delivered
        .iter()
        .map(|e| e["eventType"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(kinds, ["Started", "Updated", "Ended"]);
    let original = generated_at.to_rfc3339_opts(SecondsFormat::Millis, true);
    for event in &delivered {
        assert_eq!(event["offline"], true, "{event}");
        assert_eq!(event["timestamp"], original.as_str(), "{event}");
    }
    assert_eq!(delivered[0]["evse"]["id"], 1);
    wait_queue_empty(&station).await;
    stop(&station, task).await;
}

#[tokio::test]
async fn an_unanswered_event_is_resent_flagged_offline_after_the_link_drops() {
    let csms = v201_csms().await;
    let clock = ManualClock::new(Utc::now());
    let (station, task) = spawn_with_clock(&csms, Arc::new(clock.clone()));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    set_variables(&csms, &[("TxCtrlr", "TxStartPoint", None, "EVConnected")]).await;

    // The CSMS reads the event but never answers it.
    csms.inject(Fault::silence("TransactionEvent"));
    send(&station, Command2::PlugIn { evse: 1 }).await;
    let first = wait_events(&csms, 1).await.remove(0);
    assert!(first.get("offline").is_none());
    clock.advance(ChronoDuration::hours(1));
    csms.drop_connection().expect("drop the connection");

    // After the automatic reconnect the same event comes again.
    let both = wait_events(&csms, 2).await;
    let mut expected = first.clone();
    expected["offline"] = json!(true);
    assert_eq!(both[1], expected, "only the offline flag differs");
    let calls = transaction_event_calls(&csms);
    assert_ne!(calls[0].message_id, calls[1].message_id);
    assert_ne!(calls[0].connection, calls[1].connection);
    wait_queue_empty(&station).await;
    stop(&station, task).await;
}

#[tokio::test]
async fn a_failed_event_is_retried_with_a_linear_backoff() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    quick_cable_sessions(&csms, None).await;

    // Two failures, then the third attempt (the default `MessageAttempts` is 3) is answered.
    csms.inject(Fault::call_error("TransactionEvent").times(2));
    send(&station, Command2::PlugIn { evse: 1 }).await;
    let tries = wait_events(&csms, 3).await;
    assert_eq!(tries[0], tries[1]);
    assert_eq!(tries[1], tries[2], "the same payload every time");
    let calls = transaction_event_calls(&csms);
    assert_eq!(calls.len(), 3);
    let ids: Vec<&str> = calls.iter().map(|c| c.message_id.as_str()).collect();
    assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
    // Waits of 1 s and 2 s (attempt number times the interval); only lower bounds are safe.
    let first_gap = calls[1].ts - calls[0].ts;
    let second_gap = calls[2].ts - calls[1].ts;
    assert!(
        first_gap >= ChronoDuration::milliseconds(900),
        "first wait {first_gap}"
    );
    assert!(
        second_gap >= ChronoDuration::milliseconds(1_800),
        "second wait {second_gap}"
    );
    wait_queue_empty(&station).await;
    assert_eq!(events(&csms).len(), 3, "nothing is sent after the success");
    stop(&station, task).await;
}

#[tokio::test]
async fn an_event_is_discarded_after_the_last_attempt() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    quick_cable_sessions(&csms, Some("2")).await;

    csms.inject(Fault::call_error("TransactionEvent").times(2));
    send(&station, Command2::PlugIn { evse: 1 }).await;
    let tries = wait_events(&csms, 2).await;
    assert_eq!(tries[0], tries[1]);
    // The second failure discards the Started event.
    wait_queue_empty(&station).await;

    // The transaction goes on: its next event continues the numbering.
    send(&station, Command2::Unplug { evse: 1 }).await;
    let all = wait_events(&csms, 3).await;
    assert_eq!(seqs(&all), [0, 0, 1]);
    assert_eq!(all[2]["eventType"], "Ended");
    wait_queue_empty(&station).await;
    assert_eq!(events(&csms).len(), 3, "the discarded event never returns");
    stop(&station, task).await;
}

#[tokio::test]
async fn a_failing_head_blocks_the_events_behind_it() {
    let csms = v201_csms().await;
    let (station, task) = booted(&csms).await;
    quick_cable_sessions(&csms, None).await;

    csms.inject(Fault::call_error("TransactionEvent"));
    send(&station, Command2::PlugIn { evse: 1 }).await;
    // The next event is generated at once, while the head waits for its retry.
    send(
        &station,
        Command2::SetChargingState {
            evse: 1,
            state: ChargingState::SuspendedEv,
        },
    )
    .await;
    let all = wait_events(&csms, 3).await;
    assert_eq!(
        seqs(&all),
        [0, 0, 1],
        "the Updated event waits for the Started retry"
    );
    let calls = transaction_event_calls(&csms);
    assert!(calls[2].ts >= calls[1].ts);
    wait_queue_empty(&station).await;
    stop(&station, task).await;
}

#[tokio::test]
async fn events_wait_for_an_accepted_registration() {
    let csms = v201_csms().await;
    csms.respond("BootNotification", |_| Reply::Result(boot_reply("Pending")));
    let (station, task) = spawn_with_clock(&csms, Arc::new(ManualClock::new(Utc::now())));
    station.connect().await.expect("connect");
    wait_snapshot(&station, "a pending registration", |s| {
        s.registration == Some(RegistrationStatus::Pending)
    })
    .await;
    let component = Component {
        evse: None,
        name: "TxCtrlr".to_owned(),
        instance: None,
        custom_data: None,
    };
    let variable = Variable {
        name: "TxStartPoint".to_owned(),
        instance: None,
        custom_data: None,
    };
    send(
        &station,
        Command2::SetVariable {
            component,
            variable,
            value: "EVConnected".to_owned(),
        },
    )
    .await;
    send(&station, Command2::PlugIn { evse: 1 }).await;
    wait_snapshot(&station, "the queued Started event", |s| {
        s.queued_events == 1
    })
    .await;

    // The boot is retried while Pending; once it has been seen again the event would have
    // gone out had the gate been open.
    csms.wait_for_call_count_after(0, "BootNotification", 2, |_| true, STEP)
        .await
        .expect("the boot is retried");
    assert!(events(&csms).is_empty(), "nothing leaves while Pending");

    csms.respond("BootNotification", |_| {
        Reply::Result(boot_reply("Accepted"))
    });
    wait_booted(&station).await;
    let delivered = wait_events(&csms, 1).await;
    assert_eq!(delivered[0]["eventType"], "Started");
    assert_eq!(delivered[0]["seqNo"], 0);
    wait_queue_empty(&station).await;
    stop(&station, task).await;
}
