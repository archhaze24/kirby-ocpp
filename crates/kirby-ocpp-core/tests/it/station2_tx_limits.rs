use std::sync::Arc;

use chrono::DateTime;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, IdTokenType, StopReason};
use kirby_ocpp_core::station2::TransactionLimit2;
use kirby_ocpp_core::{Command2, Outcome2, Station2, Station2Handle, StationConfig, SystemClock};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, Reply, SubprotocolPolicy};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::station2_support::{STEP, wait_booted, wait_snapshot};

const ENERGY_LIMIT: &str = "EnergyLimitReached";
const COST_LIMIT: &str = "CostLimitReached";

struct Session {
    csms: MockCsms,
    station: Station2Handle,
    task: JoinHandle<()>,
}

impl Session {
    async fn new(protocol: Protocol) -> Self {
        let csms = MockCsms::start(CsmsOptions {
            subprotocol: SubprotocolPolicy::Require(protocol.token().to_owned()),
            heartbeat_interval: 3600,
            ..CsmsOptions::default()
        })
        .await
        .expect("mock CSMS starts");
        csms.respond("Authorize", |_| {
            Reply::Result(json!({"idTokenInfo": {"status": "Accepted"}}))
        });
        csms.respond("TransactionEvent", |_| Reply::Result(json!({})));
        let mut config = StationConfig::new(csms.url()).without_persistence();
        protocol.token().clone_into(&mut config.ws_subprotocol);
        config.protocol = protocol;
        config.nominal_power_w = 7200;
        let (station, task) = Station2::spawn(config, Arc::new(SystemClock));
        timeout(STEP, station.connect()).await.unwrap().unwrap();
        wait_booted(&station).await;
        Self {
            csms,
            station,
            task,
        }
    }

    async fn stop(self) {
        timeout(STEP, self.station.shutdown())
            .await
            .unwrap()
            .unwrap();
        timeout(STEP, self.task).await.unwrap().unwrap();
        self.csms.shutdown().await;
    }

    async fn send(&self, command: Command2) -> Outcome2 {
        timeout(STEP, self.station.send(command))
            .await
            .unwrap()
            .unwrap()
    }

    async fn call(&self, action: &str, payload: Value) -> Value {
        match self.csms.send_call(action, payload).await.unwrap() {
            Frame::CallResult { payload, .. } => payload,
            other => panic!("{action} returned {other:?}"),
        }
    }

    async fn start(&self) -> Value {
        let mark = self.csms.mark();
        self.send(Command2::PlugIn { evse: 1 }).await;
        self.send(Command2::PresentToken {
            evse: Some(1),
            id_token: "TAG".to_owned(),
            token_type: IdTokenType::Central,
        })
        .await;
        self.event(mark, |p| p["eventType"] == "Started").await
    }

    async fn event(&self, mark: usize, predicate: impl Fn(&Value) -> bool + Send) -> Value {
        self.csms
            .wait_for_call_after(mark, "TransactionEvent", predicate, STEP)
            .await
            .unwrap()
    }

    async fn trigger(&self, mark: usize, reason: &str) -> Value {
        self.event(mark, |p| p["triggerReason"] == reason).await
    }

    fn started_answer(&self, answer: Value) {
        self.csms.respond("TransactionEvent", move |p| {
            Reply::Result(if p["eventType"] == "Started" {
                answer.clone()
            } else {
                json!({})
            })
        });
    }

    async fn sample_answer(&self, answer: Value) {
        self.csms.respond("TransactionEvent", move |p| {
            Reply::Result(if p["triggerReason"] == "MeterValuePeriodic" {
                answer.clone()
            } else {
                json!({})
            })
        });
        self.sample().await;
    }

    async fn sample(&self) -> Value {
        let mark = self.csms.mark();
        self.send(Command2::SampleMeter { evse: 1 }).await;
        let sample = self.trigger(mark, "MeterValuePeriodic").await;
        let seq = sample["seqNo"].as_i64().unwrap();
        wait_snapshot(
            &self.station,
            "the meter sample response and resulting events",
            |s| {
                s.queued_events == 0
                    && s.evse(1)
                        .and_then(|e| e.transaction.as_ref())
                        .is_some_and(|tx| i64::from(tx.next_seq_no) > seq)
            },
        )
        .await;
        sample
    }

    async fn set_variable(&self, component: &str, variable: &str, value: &str) {
        let reply = self
            .call(
                "SetVariables",
                json!({"setVariableData": [{
                    "component": {"name": component},
                    "variable": {"name": variable},
                    "attributeValue": value,
                }]}),
            )
            .await;
        assert_eq!(reply["setVariableResult"][0]["attributeStatus"], "Accepted");
    }

    fn limit_set_count(&self) -> usize {
        self.csms
            .received()
            .iter()
            .filter(|call| {
                call.action == "TransactionEvent" && call.payload["triggerReason"] == "LimitSet"
            })
            .count()
    }
}

fn assert_limit_set(event: &Value, fields: &Value) {
    assert_eq!(event["eventType"], "Updated");
    assert_eq!(&event["transactionInfo"]["transactionLimit"], fields);
}

fn assert_suspended(event: &Value) {
    assert_eq!(event["eventType"], "Updated");
    assert_eq!(event["transactionInfo"]["chargingState"], "SuspendedEVSE");
}

fn energy_sample(event: &Value, context: &str) -> f64 {
    event["meterValue"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|meter| meter["sampledValue"].as_array().unwrap())
        .find(|sample| {
            sample["context"] == context && sample["measurand"] == "Energy.Active.Import.Register"
        })
        .and_then(|sample| sample["value"].as_f64())
        .expect("an energy reading")
}

#[tokio::test]
async fn tx_limit_energy_suspends_and_clamps_the_meter() {
    let s = Session::new(Protocol::V21).await;
    s.started_answer(json!({"transactionLimit": {"maxEnergy": 3.0}}));
    let started = s.start().await;
    assert_limit_set(&s.trigger(0, "LimitSet").await, &json!({"maxEnergy": 3.0}));
    assert_suspended(&s.trigger(0, ENERGY_LIMIT).await);
    let sample = s.sample().await;
    assert_eq!(
        energy_sample(&sample, "Sample.Periodic") - energy_sample(&started, "Transaction.Begin"),
        3.0
    );
    assert!(
        sample["meterValue"][0]["sampledValue"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| {
                v["measurand"] == "Power.Active.Import" && v["value"].as_f64() == Some(0.0)
            })
    );
    let snapshot = s.station.snapshot();
    let evse = snapshot.evse(1).unwrap();
    assert_eq!(evse.offered_w, Some(0.0));
    assert_eq!(evse.limit_reached.as_deref(), Some(ENERGY_LIMIT));
    assert_eq!(evse.transaction_limit.unwrap().max_energy_wh, Some(3.0));
    assert_eq!(s.limit_set_count(), 1);
    s.send(Command2::StopTransaction {
        evse: 1,
        reason: StopReason::Local,
    })
    .await;
    s.send(Command2::Unplug { evse: 1 }).await;

    let mark = s.csms.mark();
    let started = s.start().await;
    assert_suspended(&s.trigger(mark, ENERGY_LIMIT).await);
    assert_eq!(
        energy_sample(&s.sample().await, "Sample.Periodic")
            - energy_sample(&started, "Transaction.Begin"),
        3.0
    );
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_energy_ends_at_an_energy_transfer_stop_point() {
    let s = Session::new(Protocol::V21).await;
    s.set_variable("TxCtrlr", "TxStopPoint", "EnergyTransfer")
        .await;
    s.started_answer(json!({"transactionLimit": {"maxEnergy": 1.0}}));
    s.start().await;
    let ended = s.trigger(0, ENERGY_LIMIT).await;
    assert_eq!(ended["eventType"], "Ended");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], ENERGY_LIMIT);
    assert_eq!(energy_sample(&ended, "Transaction.End"), 1.0);
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_time_is_relative_to_started() {
    let s = Session::new(Protocol::V21).await;
    s.started_answer(json!({"transactionLimit": {"maxTime": 2}}));
    let started = s.start().await;
    let reached = s.trigger(0, "TimeLimitReached").await;
    assert_suspended(&reached);
    let ts =
        |event: &Value| DateTime::parse_from_rfc3339(event["timestamp"].as_str().unwrap()).unwrap();
    assert!(ts(&reached) - ts(&started) >= chrono::Duration::seconds(2));
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_cost_from_a_later_transaction_event_response() {
    let s = Session::new(Protocol::V21).await;
    s.started_answer(json!({"transactionLimit": {"maxCost": 2.5}}));
    s.start().await;
    s.trigger(0, "LimitSet").await;
    s.sample_answer(json!({"totalCost": 2.5})).await;
    assert_suspended(&s.trigger(0, COST_LIMIT).await);
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_cost_from_cost_updated_ends_with_local_out_of_credit() {
    let s = Session::new(Protocol::V21).await;
    s.set_variable("TxCtrlr", "TxStopPoint", "EnergyTransfer")
        .await;
    s.started_answer(json!({"transactionLimit": {"maxCost": 2.5}}));
    let started = s.start().await;
    s.trigger(0, "LimitSet").await;
    assert_eq!(
        s.call(
            "CostUpdated",
            json!({
                "transactionId": started["transactionInfo"]["transactionId"], "totalCost": 3.0,
            })
        )
        .await,
        json!({})
    );
    let ended = s.trigger(0, COST_LIMIT).await;
    assert_eq!(ended["eventType"], "Ended");
    assert_eq!(
        ended["transactionInfo"]["stoppedReason"],
        "LocalOutOfCredit"
    );
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_raise_resumes_in_the_limit_set_event() {
    let s = Session::new(Protocol::V21).await;
    s.started_answer(json!({"transactionLimit": {"maxEnergy": 1.0}}));
    s.start().await;
    assert_suspended(&s.trigger(0, ENERGY_LIMIT).await);
    let mark = s.csms.mark();
    s.sample_answer(json!({"transactionLimit": {"maxEnergy": 1000.0}}))
        .await;
    let updated = s.trigger(mark, "LimitSet").await;
    assert_limit_set(&updated, &json!({"maxEnergy": 1000.0}));
    assert_eq!(updated["transactionInfo"]["chargingState"], "Charging");
    wait_snapshot(&s.station, "energy delivery resumes", |snap| {
        snap.evse(1)
            .and_then(|e| e.transaction.as_ref())
            .is_some_and(|tx| tx.charging_state == ChargingState::Charging && tx.energy_wh > 1)
    })
    .await;
    assert_eq!(s.limit_set_count(), 2);
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_echo_does_not_generate_a_second_limit_set() {
    let s = Session::new(Protocol::V21).await;
    s.csms.respond("TransactionEvent", |p| {
        Reply::Result(
            if p["eventType"] == "Started" || p["triggerReason"] == "LimitSet" {
                json!({"transactionLimit": {"maxTime": 60}})
            } else {
                json!({})
            },
        )
    });
    s.start().await;
    assert_limit_set(&s.trigger(0, "LimitSet").await, &json!({"maxTime": 60}));
    s.send(Command2::SampleMeter { evse: 1 }).await;
    s.trigger(0, "MeterValuePeriodic").await;
    assert_eq!(s.limit_set_count(), 1);
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_ignores_soc_and_echoes_only_supported_fields() {
    let s = Session::new(Protocol::V21).await;
    s.started_answer(json!({"transactionLimit": {"maxSoC": 80}}));
    s.start().await;
    s.sample().await;
    assert_eq!(s.limit_set_count(), 0);
    s.sample_answer(json!({"transactionLimit": {
        "maxSoC": 80, "maxTime": 60.0, "customData": {"vendorId": "test"},
    }}))
    .await;
    assert_limit_set(&s.trigger(0, "LimitSet").await, &json!({"maxTime": 60}));
    assert_eq!(s.limit_set_count(), 1);
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_below_delivered_energy_or_negative_suspends_when_set() {
    for max_energy in [5.0_f64, -1.0] {
        let s = Session::new(Protocol::V21).await;
        s.start().await;
        if max_energy > 0.0 {
            s.send(Command2::AdvanceMeter {
                evse: 1,
                wh_delta: 20,
            })
            .await;
        }
        let mark = s.csms.mark();
        s.sample_answer(json!({"transactionLimit": {"maxEnergy": max_energy}}))
            .await;
        let set = s.trigger(mark, "LimitSet").await;
        let reached = s.trigger(mark, ENERGY_LIMIT).await;
        assert_eq!(
            reached["seqNo"].as_i64(),
            set["seqNo"].as_i64().map(|n| n + 1)
        );
        assert_suspended(&reached);
        assert_eq!(
            energy_sample(&s.sample().await, "Sample.Periodic"),
            max_energy.max(0.0)
        );
        s.stop().await;
    }
}

#[tokio::test]
async fn tx_limit_device_model_and_response_are_protocol_scoped() {
    for protocol in [Protocol::V201, Protocol::V21] {
        let s = Session::new(protocol).await;
        let key =
            json!({"component": {"name": "TxCtrlr"}, "variable": {"name": "SupportedLimits"}});
        let answer = s
            .call("GetVariables", json!({"getVariableData": [key]}))
            .await;
        let row = &answer["getVariableResult"][0];
        if protocol == Protocol::V21 {
            assert_eq!(row["attributeStatus"], "Accepted");
            assert_eq!(row["attributeValue"], "maxEnergy,maxTime,maxCost");
        } else {
            assert_eq!(row["attributeStatus"], "UnknownVariable");
            s.started_answer(json!({"totalCost": 3.5}));
            s.start().await;
            s.sample().await;
            assert_eq!(s.limit_set_count(), 0);
            let snapshot = s.station.snapshot();
            let evse = snapshot.evse(1).unwrap();
            assert_eq!(evse.transaction_limit, None);
            assert_eq!(evse.total_cost, Some(3.5));
        }
        s.stop().await;
    }
}

#[tokio::test]
async fn cost_updated_answers_with_an_empty_confirmation_on_201() {
    let s = Session::new(Protocol::V201).await;
    let started = s.start().await;
    assert_eq!(
        s.call(
            "CostUpdated",
            json!({
                "transactionId": started["transactionInfo"]["transactionId"], "totalCost": 12.34,
            })
        )
        .await,
        json!({})
    );
    wait_snapshot(&s.station, "the running cost", |snap| {
        snap.evse(1).and_then(|e| e.total_cost) == Some(12.34)
    })
    .await;
    assert_eq!(
        s.call(
            "CostUpdated",
            json!({"transactionId": "unknown", "totalCost": 99})
        )
        .await,
        json!({})
    );
    s.sample().await;
    assert_eq!(
        s.station.snapshot().evse(1).unwrap().total_cost,
        Some(12.34)
    );
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_station_command_respects_the_last_csms_values() {
    let s = Session::new(Protocol::V21).await;
    let limit = TransactionLimit2 {
        max_time_s: Some(40),
        ..TransactionLimit2::default()
    };
    let set = async |limit| {
        timeout(STEP, s.station.set_transaction_limit(1, limit))
            .await
            .unwrap()
            .unwrap()
    };
    assert!(matches!(set(limit).await, Outcome2::Rejected { .. }));
    s.started_answer(json!({"transactionLimit": {"maxTime": 60, "maxCost": 10.0}}));
    s.start().await;
    s.trigger(0, "LimitSet").await;
    assert_eq!(set(limit).await, Outcome2::Done);
    s.sample().await;
    let event = s
        .csms
        .received()
        .into_iter()
        .rfind(|call| {
            call.action == "TransactionEvent" && call.payload["triggerReason"] == "LimitSet"
        })
        .unwrap()
        .payload;
    assert_limit_set(&event, &json!({"maxTime": 40}));
    assert_eq!(
        s.station.snapshot().evse(1).unwrap().transaction_limit,
        Some(TransactionLimit2 {
            max_time_s: Some(40),
            max_cost: Some(10.0),
            ..TransactionLimit2::default()
        })
    );
    for refused in [
        TransactionLimit2::default(),
        TransactionLimit2 {
            max_time_s: Some(61),
            ..limit
        },
    ] {
        assert!(matches!(set(refused).await, Outcome2::Rejected { .. }));
    }
    s.sample().await;
    assert_eq!(s.limit_set_count(), 2);
    s.stop().await;

    let s = Session::new(Protocol::V201).await;
    s.start().await;
    assert!(matches!(
        timeout(STEP, s.station.set_transaction_limit(1, limit))
            .await
            .unwrap()
            .unwrap(),
        Outcome2::Rejected { .. }
    ));
    s.sample().await;
    assert_eq!(s.limit_set_count(), 0);
    s.stop().await;
}

#[tokio::test]
async fn tx_limit_is_applied_from_an_offline_event_response() {
    let s = Session::new(Protocol::V21).await;
    s.start().await;
    s.sample().await;
    timeout(STEP, s.station.disconnect())
        .await
        .unwrap()
        .unwrap();
    s.send(Command2::SampleMeter { evse: 1 }).await;
    s.csms.respond("TransactionEvent", |p| {
        Reply::Result(if p["offline"] == true {
            json!({"transactionLimit": {"maxCost": 8.25}})
        } else {
            json!({})
        })
    });
    let mark = s.csms.mark();
    timeout(STEP, s.station.connect()).await.unwrap().unwrap();
    let sample = s.trigger(mark, "MeterValuePeriodic").await;
    assert_eq!(sample["offline"], true);
    assert_limit_set(
        &s.trigger(mark, "LimitSet").await,
        &json!({"maxCost": 8.25}),
    );
    s.stop().await;
}
