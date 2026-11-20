//! Master pass of the OCPP 2.x station (C16): a token whose group equals
//! `AuthCtrlr.MasterPassGroupId` starts no transaction and ends all running ones with
//! `stoppedReason = MasterPass`. Every test runs against 2.0.1 and 2.1.
//!
//! Waits are bounded by `STEP`; nothing sleeps for a fixed time.

use std::sync::Arc;

use chrono::Utc;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::v2::types::{Component, IdTokenType, Variable};
use kirby_ocpp_core::{
    Command2, ManualClock, Outcome2, Station2, Station2Handle, StationConfig, StationError,
};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, Reply, SubprotocolPolicy};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::station2_support::{STEP, wait_booted, wait_snapshot};

const MASTER_GROUP: &str = "MASTERGROUP";

struct Rig {
    csms: MockCsms,
    station: Station2Handle,
    task: JoinHandle<()>,
}

impl Rig {
    async fn new(protocol: Protocol) -> Self {
        let token = protocol.token().to_owned();
        let csms = MockCsms::start(CsmsOptions {
            subprotocol: SubprotocolPolicy::Require(token.clone()),
            ..CsmsOptions::default()
        })
        .await
        .expect("mock CSMS starts");
        csms.respond("BootNotification", |_| {
            Reply::Result(json!({
                "status": "Accepted",
                "currentTime": "2026-03-01T12:00:00Z",
                "interval": 3600,
            }))
        });
        // `CARD*` tokens are ordinary, `MASTER*` ones carry the master pass group.
        csms.respond("Authorize", |payload| {
            let id = payload["idToken"]["idToken"].as_str().unwrap_or_default();
            let mut info = json!({"status": "Accepted"});
            if id.starts_with("MASTER") {
                info["groupIdToken"] = json!({"idToken": MASTER_GROUP, "type": "Central"});
            }
            Reply::Result(json!({"idTokenInfo": info}))
        });
        let mut config = StationConfig::new(csms.url())
            .with_connectors(2)
            .with_call_timeout_ms(2_000)
            .with_heartbeat_interval_secs(3600)
            .with_reconnect(ReconnectConfig {
                enabled: true,
                initial_delay_ms: 30,
                max_delay_ms: 100,
                max_attempts: 0,
            })
            .without_persistence();
        config.ws_ping_interval_secs = 0;
        config.ws_subprotocol = token;
        config.protocol = protocol;
        let (station, task) = Station2::spawn(config, Arc::new(ManualClock::new(Utc::now())));
        station.connect().await.expect("connect");
        wait_booted(&station).await;
        Self {
            csms,
            station,
            task,
        }
    }

    async fn stop(self) {
        let _ = self.station.shutdown().await;
        let _ = self.task.await;
        self.csms.shutdown().await;
    }

    async fn send(&self, command: Command2) -> Outcome2 {
        self.station.send(command).await.expect("command accepted")
    }

    async fn present(&self, evse: Option<u32>, token: &str) -> Result<Outcome2, StationError> {
        self.station
            .send(Command2::PresentToken {
                evse,
                id_token: token.to_owned(),
                token_type: IdTokenType::Iso14443,
            })
            .await
    }

    async fn set_master_group(&self, value: &str) {
        self.send(Command2::SetVariable {
            component: Component {
                evse: None,
                name: "AuthCtrlr".to_owned(),
                instance: None,
                custom_data: None,
            },
            variable: Variable {
                name: "MasterPassGroupId".to_owned(),
                instance: None,
                custom_data: None,
            },
            value: value.to_owned(),
        })
        .await;
    }

    /// Plugs in and authorizes with `token` on `evse`; waits for the transaction.
    async fn start_on(&self, evse: u32, token: &str) {
        self.send(Command2::PlugIn { evse }).await;
        let outcome = self.present(Some(evse), token).await.expect("accepted");
        assert!(
            matches!(outcome, Outcome2::TransactionStarted { .. }),
            "{outcome:?}"
        );
    }

    fn events_of(&self, kind: &str) -> Vec<Value> {
        self.csms
            .received()
            .into_iter()
            .filter(|call| call.action == "TransactionEvent" && call.payload["eventType"] == kind)
            .map(|call| call.payload)
            .collect()
    }

    async fn wait_events(&self, kind: &str, count: usize) -> Vec<Value> {
        let wanted = kind.to_owned();
        self.csms
            .wait_for_call_count_after(
                0,
                "TransactionEvent",
                count,
                move |payload| payload["eventType"] == wanted.as_str(),
                STEP,
            )
            .await
            .expect("the events arrive");
        self.events_of(kind)
    }

    async fn wait_no_transactions(&self) {
        wait_snapshot(&self.station, "all transactions ended", |s| {
            s.evses.iter().all(|e| e.transaction.is_none())
        })
        .await;
    }
}

const VERSIONS: [Protocol; 2] = [Protocol::V201, Protocol::V21];

#[tokio::test]
async fn without_a_master_pass_group_a_group_token_is_an_ordinary_token() {
    for protocol in VERSIONS {
        let rig = Rig::new(protocol).await;
        rig.send(Command2::PlugIn { evse: 1 }).await;
        let outcome = rig.present(Some(1), "MASTER1").await.expect("accepted");
        assert!(
            matches!(outcome, Outcome2::TransactionStarted { evse: 1, .. }),
            "{protocol:?}: {outcome:?}"
        );
        assert_eq!(rig.events_of("Ended").len(), 0);
        rig.stop().await;
    }
}

#[tokio::test]
async fn a_master_pass_ends_all_transactions_with_the_master_pass_reason() {
    for protocol in VERSIONS {
        let rig = Rig::new(protocol).await;
        rig.set_master_group(MASTER_GROUP).await;
        rig.start_on(1, "CARD1").await;
        rig.start_on(2, "CARD2").await;

        // No EVSE is free: the master pass is still accepted and acts on every EVSE.
        let outcome = rig.present(None, "MASTER1").await.expect("accepted");
        assert!(
            matches!(outcome, Outcome2::Done),
            "{protocol:?}: {outcome:?}"
        );
        let ended = rig.wait_events("Ended", 2).await;
        rig.wait_no_transactions().await;
        for event in &ended {
            assert_eq!(event["transactionInfo"]["stoppedReason"], "MasterPass");
            assert_eq!(event["triggerReason"], "StopAuthorized");
            assert_eq!(
                event["idToken"],
                json!({"idToken": "MASTER1", "type": "ISO14443"})
            );
        }
        // One `Ended` per transaction that was running.
        let ids = |events: &[Value]| {
            let mut ids: Vec<String> = events
                .iter()
                .filter_map(|e| e["transactionInfo"]["transactionId"].as_str())
                .map(str::to_owned)
                .collect();
            ids.sort_unstable();
            ids
        };
        let started = rig.wait_events("Started", 2).await;
        assert_eq!(ids(&started), ids(&ended), "{protocol:?}");
        rig.stop().await;
    }
}

#[tokio::test]
async fn a_master_pass_ends_the_transaction_even_when_the_stop_points_would_not() {
    for protocol in VERSIONS {
        let rig = Rig::new(protocol).await;
        rig.set_master_group(MASTER_GROUP).await;
        rig.send(Command2::SetVariable {
            component: Component {
                evse: None,
                name: "TxCtrlr".to_owned(),
                instance: None,
                custom_data: None,
            },
            variable: Variable {
                name: "TxStopPoint".to_owned(),
                instance: None,
                custom_data: None,
            },
            value: "EVConnected".to_owned(),
        })
        .await;
        rig.start_on(1, "CARD1").await;
        rig.present(None, "MASTER1").await.expect("accepted");
        let ended = rig.wait_events("Ended", 1).await;
        assert_eq!(
            ended[0]["transactionInfo"]["stoppedReason"], "MasterPass",
            "{protocol:?}"
        );
        rig.stop().await;
    }
}

#[tokio::test]
async fn a_master_pass_starts_no_transaction() {
    for protocol in VERSIONS {
        let rig = Rig::new(protocol).await;
        rig.set_master_group(MASTER_GROUP).await;
        rig.send(Command2::PlugIn { evse: 1 }).await;
        let outcome = rig.present(Some(1), "MASTER1").await.expect("accepted");
        assert!(
            matches!(outcome, Outcome2::Done),
            "{protocol:?}: {outcome:?}"
        );
        // A later ordinary token proves the station handled the earlier command; the
        // transaction it starts is the first one.
        rig.present(Some(1), "CARD1").await.expect("accepted");
        let started = rig.wait_events("Started", 1).await;
        assert_eq!(
            started[0]["idToken"],
            json!({"idToken": "CARD1", "type": "ISO14443"}),
            "{protocol:?}"
        );
        assert_eq!(rig.events_of("Ended").len(), 0);
        rig.stop().await;
    }
}

#[tokio::test]
async fn a_master_pass_from_the_cache_ends_transactions_without_asking_again() {
    for protocol in VERSIONS {
        let rig = Rig::new(protocol).await;
        rig.set_master_group(MASTER_GROUP).await;
        rig.start_on(1, "CARD1").await;
        // The bare `Authorize` fills the cache with the group of the token.
        rig.send(Command2::Authorize {
            id_token: "MASTER1".to_owned(),
            token_type: IdTokenType::Iso14443,
        })
        .await;
        let asked = rig
            .csms
            .received()
            .iter()
            .filter(|c| c.action == "Authorize")
            .count();
        rig.present(None, "MASTER1").await.expect("accepted");
        let ended = rig.wait_events("Ended", 1).await;
        assert_eq!(
            ended[0]["transactionInfo"]["stoppedReason"], "MasterPass",
            "{protocol:?}"
        );
        let asked_after = rig
            .csms
            .received()
            .iter()
            .filter(|c| c.action == "Authorize")
            .count();
        assert_eq!(asked, asked_after, "{protocol:?}: the cache answers");
        rig.stop().await;
    }
}

#[tokio::test]
async fn a_remote_start_with_a_master_pass_token_starts_nothing() {
    for protocol in VERSIONS {
        let rig = Rig::new(protocol).await;
        rig.set_master_group(MASTER_GROUP).await;
        rig.send(Command2::PlugIn { evse: 1 }).await;
        let answer = rig
            .csms
            .send_call(
                "RequestStartTransaction",
                json!({
                    "idToken": {"idToken": "MASTER1", "type": "ISO14443"},
                    "remoteStartId": 7,
                    "evseId": 1,
                }),
            )
            .await
            .expect("answered");
        drop(answer);
        // Once the Authorize for the token went out and a following ordinary start works, the
        // master pass request is behind us.
        rig.csms
            .wait_for_call_count_after(0, "Authorize", 1, |_| true, STEP)
            .await
            .expect("the token is checked");
        rig.present(Some(1), "CARD1").await.expect("accepted");
        let started = rig.wait_events("Started", 1).await;
        assert_eq!(
            started[0]["idToken"],
            json!({"idToken": "CARD1", "type": "ISO14443"}),
            "{protocol:?}"
        );
        rig.stop().await;
    }
}
