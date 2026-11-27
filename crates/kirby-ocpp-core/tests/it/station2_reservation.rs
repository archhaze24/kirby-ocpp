//! Reservations of the OCPP 2.x station: `ReserveNow`, `CancelReservation`, use of a reserved
//! EVSE, expiry and removal, `ReservationCtrlr` and persistence (H01 to H04).
//!
//! Tests drive the station through the mock CSMS and the station clock. Waits are bounded by
//! `STEP`; nothing sleeps for a fixed time. Tokens of the type `Central` need no `Authorize`.

use std::path::Path;
use std::sync::Arc;

use chrono::{Duration, SecondsFormat, Utc};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, IdTokenType, StopReason};
use kirby_ocpp_core::station::Event;
use kirby_ocpp_core::station2::{IdTokenRef, Reservation, ReservationPersist};
use kirby_ocpp_core::store::{FileKind, LoadOutcome2, PersistedStation2, StationStore};
use kirby_ocpp_core::{Command2, ManualClock, Outcome2, Station2, Station2Handle, StationConfig};
use kirby_ocpp_testkit::{CsmsOptions, MockCsms, Reply, SubprotocolPolicy};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::station2_remote::Rig;
use super::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};
use super::station21::v21_csms;

fn central(id: &str) -> Value {
    json!({"idToken": id, "type": "Central"})
}

fn stamp(at: chrono::DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn reserve_body(id: i64, evse: Option<u32>, token: &str, expiry: &str) -> Value {
    let mut body = json!({"id": id, "expiryDateTime": expiry, "idToken": central(token)});
    if let Some(evse) = evse {
        body["evseId"] = json!(evse);
    }
    body
}

impl Rig {
    /// The station time `by` from now, as the wire text.
    fn in_future(&self, by: Duration) -> String {
        use kirby_ocpp_core::Clock;
        stamp(self.clock.now_utc() + by)
    }

    /// A `ReserveNow` of `token` lasting an hour; the answer status.
    async fn reserve(&self, id: i64, evse: Option<u32>, token: &str) -> String {
        let expiry = self.in_future(Duration::hours(1));
        self.status_of("ReserveNow", reserve_body(id, evse, token, &expiry))
            .await
    }

    async fn cancel(&self, id: i64) -> String {
        self.status_of("CancelReservation", json!({"reservationId": id}))
            .await
    }

    /// Waits until the CSMS has `count` status notifications with `status` for `evse`.
    async fn wait_status(&self, evse: u32, status: &str, count: usize) {
        self.csms
            .wait_for_call_count_after(
                0,
                "StatusNotification",
                count,
                |p| p["evseId"] == evse && p["connectorStatus"] == status,
                STEP,
            )
            .await
            .unwrap_or_else(|_| panic!("EVSE {evse} never reported {status} {count} time(s)"));
    }

    /// How many status notifications with `status` for `evse` the CSMS has. Call
    /// [`Self::settle`] first when the notifications are still on their way.
    fn status_count(&self, evse: u32, status: &str) -> usize {
        self.calls("StatusNotification")
            .iter()
            .filter(|p| p["evseId"] == evse && p["connectorStatus"] == status)
            .count()
    }

    /// A round trip: everything the station sent before it has reached the CSMS afterwards.
    async fn settle(&self) {
        self.call("GetLocalListVersion", json!({})).await;
    }

    async fn present_central(&self, evse: Option<u32>, token: &str) -> Outcome2 {
        self.send(Command2::PresentToken {
            evse,
            id_token: token.to_owned(),
            token_type: IdTokenType::Central,
        })
        .await
    }

    async fn wait_update(&self, id: i64, status: &str) {
        self.csms
            .wait_for_call_count_after(
                0,
                "ReservationStatusUpdate",
                1,
                |p| p["reservationId"] == id && p["reservationUpdateStatus"] == status,
                STEP,
            )
            .await
            .unwrap_or_else(|_| panic!("no {status} update for reservation {id}"));
    }
}

fn authorized_status(outcome: &Outcome2) -> &str {
    match outcome {
        Outcome2::Authorized { status, .. } => status,
        other => panic!("expected an Authorized outcome, got {other:?}"),
    }
}

// ---- ReservationCtrlr --------------------------------------------------------------------

#[tokio::test]
async fn the_variables_describe_the_reservation_support() {
    let rig = Rig::new().await;
    let read = |name: &'static str| {
        super::station2_local_list::variable_of(&rig.csms, "ReservationCtrlr", name)
    };
    assert_eq!(read("Enabled").await, "true");
    assert_eq!(read("Available").await, "true");
    assert_eq!(read("NonEvseSpecific").await, "true");
    let set = |name: &str, value: &str| {
        rig.call(
            "SetVariables",
            json!({"setVariableData": [{
                "attributeValue": value,
                "component": {"name": "ReservationCtrlr"},
                "variable": {"name": name},
            }]}),
        )
    };
    let reply = set("Available", "false").await;
    assert_eq!(reply["setVariableResult"][0]["attributeStatus"], "Rejected");
    let reply = set("Enabled", "false").await;
    assert_eq!(reply["setVariableResult"][0]["attributeStatus"], "Accepted");
    assert_eq!(read("Enabled").await, "false");
    rig.stop().await;
}

// ---- H01: ReserveNow ---------------------------------------------------------------------

#[tokio::test]
async fn a_reserved_evse_reports_reserved_and_the_other_stays_available() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(1, Some(1), "A").await, "Accepted");
    rig.wait_status(1, "Reserved", 1).await;
    rig.settle().await;
    assert_eq!(rig.status_count(2, "Reserved"), 0);
    rig.stop().await;
}

#[tokio::test]
async fn the_snapshot_shows_a_reservation_until_it_is_cancelled() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(7, Some(1), "ABC").await, "Accepted");
    let held = wait_snapshot(&rig.station, "the reservation", |s| {
        s.evse(1).is_some_and(|e| e.reservation.is_some())
    })
    .await;
    let view = held
        .evse(1)
        .and_then(|e| e.reservation.clone())
        .expect("held");
    assert_eq!((view.id, view.id_token.as_str()), (7, "ABC"));
    assert_eq!(held.evse(2).map(|e| e.reservation.is_none()), Some(true));
    assert_eq!(rig.cancel(7).await, "Accepted");
    wait_snapshot(&rig.station, "the cancellation", |s| {
        s.evse(1).is_some_and(|e| e.reservation.is_none())
    })
    .await;
    rig.stop().await;
}

#[tokio::test]
async fn a_reservation_for_any_evse_shows_once_it_needs_every_free_evse() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(1, None, "A").await, "Accepted");
    rig.settle().await;
    // One reservation, two free EVSEs: nothing is held yet.
    assert_eq!(
        rig.status_count(1, "Reserved") + rig.status_count(2, "Reserved"),
        0
    );
    assert_eq!(rig.reserve(2, None, "B").await, "Accepted");
    rig.wait_status(1, "Reserved", 1).await;
    rig.wait_status(2, "Reserved", 1).await;
    // H01.FR.11: no free EVSE is left for a third.
    assert_eq!(rig.reserve(3, None, "C").await, "Occupied");
    assert_eq!(rig.reserve(4, Some(2), "C").await, "Occupied");
    rig.stop().await;
}

#[tokio::test]
async fn a_connector_type_the_station_has_is_accepted_and_another_is_rejected() {
    let rig = Rig::new().await;
    let expiry = rig.in_future(Duration::hours(1));
    let mut body = reserve_body(1, None, "A", &expiry);
    body["connectorType"] = json!("cType2");
    assert_eq!(rig.status_of("ReserveNow", body.clone()).await, "Accepted");
    body["id"] = json!(2);
    body["connectorType"] = json!("cCCS2");
    let reply = rig.call("ReserveNow", body).await;
    assert_eq!(reply["status"], "Rejected");
    assert_eq!(reply["statusInfo"]["reasonCode"], "NoSuchConnectorType");
    rig.stop().await;
}

#[tokio::test]
async fn a_known_id_replaces_the_reservation() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(7, Some(1), "A").await, "Accepted");
    rig.wait_status(1, "Reserved", 1).await;
    // H01.FR.02: the same id moves to the other EVSE.
    assert_eq!(rig.reserve(7, Some(2), "A").await, "Accepted");
    rig.wait_status(2, "Reserved", 1).await;
    rig.wait_status(1, "Available", 2).await;
    // One reservation only.
    assert_eq!(rig.cancel(7).await, "Accepted");
    assert_eq!(rig.cancel(7).await, "Rejected");
    rig.stop().await;
}

#[tokio::test]
async fn an_evse_in_use_or_reserved_answers_occupied() {
    let rig = Rig::new().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    assert_eq!(rig.reserve(1, Some(1), "A").await, "Occupied");
    assert_eq!(rig.reserve(2, Some(2), "A").await, "Accepted");
    assert_eq!(rig.reserve(3, Some(2), "B").await, "Occupied");
    // Every targeted EVSE is occupied or reserved.
    assert_eq!(rig.reserve(4, None, "B").await, "Occupied");
    rig.stop().await;
}

#[tokio::test]
async fn a_faulted_evse_answers_faulted() {
    let rig = Rig::new().await;
    rig.send(Command2::Fault {
        evse: 2,
        connector: None,
    })
    .await;
    assert_eq!(rig.reserve(1, Some(2), "A").await, "Faulted");
    // H01.FR.12 applies when all targeted EVSEs are faulted; one is still free here.
    assert_eq!(rig.reserve(2, None, "A").await, "Accepted");
    rig.send(Command2::Fault {
        evse: 1,
        connector: None,
    })
    .await;
    assert_eq!(rig.reserve(3, None, "B").await, "Faulted");
    rig.stop().await;
}

#[tokio::test]
async fn an_unavailable_evse_answers_unavailable() {
    let rig = Rig::new().await;
    for evse in [1, 2] {
        let status = rig
            .status_of(
                "ChangeAvailability",
                json!({"operationalStatus": "Inoperative", "evse": {"id": evse}}),
            )
            .await;
        assert_eq!(status, "Accepted");
    }
    assert_eq!(rig.reserve(1, Some(1), "A").await, "Unavailable");
    assert_eq!(rig.reserve(2, None, "A").await, "Unavailable");
    rig.stop().await;
}

#[tokio::test]
async fn bad_requests_are_rejected() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(1, Some(9), "A").await, "Rejected");
    let reply = rig
        .call(
            "ReserveNow",
            reserve_body(2, Some(0), "A", &rig.in_future(Duration::hours(1))),
        )
        .await;
    assert_eq!(reply["status"], "Rejected");
    // An expiry that has passed.
    let past = rig.in_future(Duration::seconds(-1));
    let reply = rig
        .call("ReserveNow", reserve_body(3, None, "A", &past))
        .await;
    assert_eq!(reply["status"], "Rejected");
    assert_eq!(reply["statusInfo"]["reasonCode"], "AlreadyExpired");
    rig.stop().await;
}

#[tokio::test]
async fn disabled_reservations_are_rejected_and_ignored() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(1, Some(1), "A").await, "Accepted");
    rig.wait_status(1, "Reserved", 1).await;
    rig.set_variable("ReservationCtrlr", "Enabled", "false")
        .await;
    // Nothing is held and nothing is accepted; the entry is kept.
    rig.wait_status(1, "Available", 2).await;
    let reply = rig
        .call(
            "ReserveNow",
            reserve_body(2, Some(2), "A", &rig.in_future(Duration::hours(1))),
        )
        .await;
    assert_eq!(reply["status"], "Rejected");
    assert_eq!(reply["statusInfo"]["reasonCode"], "ReservationDisabled");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let outcome = rig.present_central(Some(1), "B").await;
    assert!(
        matches!(outcome, Outcome2::TransactionStarted { .. }),
        "{outcome:?}"
    );
    // Enabled again: the entry holds the EVSE again once it is free.
    rig.send(Command2::StopTransaction {
        evse: 1,
        reason: StopReason::Local,
    })
    .await;
    rig.send(Command2::Unplug { evse: 1 }).await;
    rig.set_variable("ReservationCtrlr", "Enabled", "true")
        .await;
    rig.wait_status(1, "Reserved", 2).await;
    rig.stop().await;
}

// ---- H02: CancelReservation --------------------------------------------------------------

#[tokio::test]
async fn cancelling_frees_the_evse_and_an_unknown_id_is_rejected() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(4, Some(1), "A").await, "Accepted");
    rig.wait_status(1, "Reserved", 1).await;
    assert_eq!(rig.cancel(4).await, "Accepted");
    // H02.FR.04: every connector of the EVSE is available again.
    rig.wait_status(1, "Available", 2).await;
    let reply = rig
        .call("CancelReservation", json!({"reservationId": 4}))
        .await;
    assert_eq!(reply["status"], "Rejected");
    assert_eq!(rig.cancel(99).await, "Rejected");
    // The EVSE takes a reservation for someone else again.
    assert_eq!(rig.reserve(5, Some(1), "B").await, "Accepted");
    rig.stop().await;
}

// ---- H03: use a reserved EVSE ------------------------------------------------------------

#[tokio::test]
async fn any_evse_reservation_blocks_an_unrelated_token_after_plug_in() {
    tokio::time::timeout(STEP * 4, async {
        let csms = v201_csms().await;
        let clock = ManualClock::new(Utc::now());
        let mut config = StationConfig::new(csms.url())
            .with_connectors(1)
            .without_persistence();
        config.protocol = Protocol::V201;
        Protocol::V201
            .token()
            .clone_into(&mut config.ws_subprotocol);
        let (station, task) = Station2::spawn(config, Arc::new(clock.clone()));
        timeout(STEP, station.connect()).await.unwrap().unwrap();
        wait_booted(&station).await;
        let expiry = stamp(clock_now(&clock) + Duration::hours(1));
        assert_eq!(
            call_status(&csms, "ReserveNow", reserve_body(53, None, "A", &expiry)).await,
            "Accepted"
        );
        let present = |id: &str| {
            timeout(
                STEP,
                station.send(Command2::PresentToken {
                    evse: Some(1),
                    id_token: id.to_owned(),
                    token_type: IdTokenType::Central,
                }),
            )
        };
        assert_eq!(
            authorized_status(&present("B").await.unwrap().unwrap()),
            "NotAtThisLocation"
        );
        timeout(STEP, station.send(Command2::PlugIn { evse: 1 }))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            authorized_status(&present("B").await.unwrap().unwrap()),
            "NotAtThisLocation"
        );
        let outcome = present("A").await.unwrap().unwrap();
        assert!(matches!(outcome, Outcome2::TransactionStarted { .. }));
        csms.wait_for_call_count_after(
            0,
            "TransactionEvent",
            1,
            |p| p["eventType"] == "Started" && p["reservationId"] == 53,
            STEP,
        )
        .await
        .expect("Started carries the reservation");
        assert_eq!(
            call_status(&csms, "CancelReservation", json!({"reservationId": 53})).await,
            "Rejected"
        );
        wait_snapshot(&station, "EVSE 1 charging", |s| {
            s.evses
                .first()
                .and_then(|e| e.transaction.as_ref())
                .is_some_and(|tx| tx.charging_state == ChargingState::Charging)
        })
        .await;
        super::station2_transactions::stop(&station, task).await;
    })
    .await
    .expect("scenario completes");
}

#[tokio::test]
async fn canceling_pending_authorization_returns_the_reservation() {
    tokio::time::timeout(STEP * 4, async {
        let rig = Rig::new().await;
        rig.set_variable("TxCtrlr", "TxStartPoint", "PowerPathClosed")
            .await;
        assert_eq!(rig.reserve(54, Some(1), "A").await, "Accepted");
        assert_eq!(
            authorized_status(&rig.present_central(Some(1), "A").await),
            "Accepted"
        );
        assert!(matches!(
            rig.present_central(Some(1), "A").await,
            Outcome2::Done
        ));
        rig.send(Command2::PlugIn { evse: 1 }).await;
        let refused = rig.present_central(Some(1), "B").await;
        assert_eq!(authorized_status(&refused), "NotAtThisLocation");
        rig.settle().await;
        assert_eq!(rig.calls("TransactionEvent"), Vec::<Value>::new());
        assert_eq!(rig.cancel(54).await, "Accepted");
        rig.stop().await;
    })
    .await
    .expect("scenario completes");
}

#[tokio::test]
async fn offline_fault_removes_reservation_even_after_fault_clears() {
    tokio::time::timeout(STEP * 4, async {
        let rig = Rig::new().await;
        assert_eq!(rig.reserve(55, Some(1), "A").await, "Accepted");
        rig.send(Command2::Disconnect).await;
        rig.send(Command2::Fault {
            evse: 1,
            connector: None,
        })
        .await;
        rig.send(Command2::ClearFault {
            evse: 1,
            connector: None,
        })
        .await;
        assert!(
            rig.station
                .snapshot()
                .evses
                .iter()
                .any(|e| { e.id == 1 && e.reservation.is_none() })
        );
        rig.send(Command2::Connect).await;
        rig.wait_update(55, "Removed").await;
        assert_eq!(rig.cancel(55).await, "Rejected");
        assert_eq!(rig.reserve(56, Some(1), "B").await, "Accepted");
        rig.stop().await;
    })
    .await
    .expect("scenario completes");
}

#[tokio::test]
async fn authorize_resolves_uncached_reservation_group_for_local_start() {
    tokio::time::timeout(STEP * 4, async {
        let rig = Rig::new().await;
        rig.csms.respond("Authorize", |_| {
            Reply::Result(
                json!({"idTokenInfo": {"status": "Accepted", "groupIdToken": central("G")}}),
            )
        });
        let expiry = rig.in_future(Duration::hours(1));
        for evse in 1..=2 {
            let mut body = reserve_body(i64::from(evse), Some(evse), "A", &expiry);
            body["groupIdToken"] = central("G");
            assert_eq!(rig.status_of("ReserveNow", body).await, "Accepted");
        }
        rig.send(Command2::PlugIn { evse: 1 }).await;
        let outcome = rig
            .send(Command2::PresentToken {
                evse: None,
                id_token: "MEMBER".to_owned(),
                token_type: IdTokenType::Iso14443,
            })
            .await;
        assert!(matches!(
            outcome,
            Outcome2::TransactionStarted { evse: 1, .. }
        ));
        assert_eq!(rig.calls("Authorize").len(), 1);
        assert_eq!(rig.wait_event("Started").await["reservationId"], 1);
        wait_snapshot(&rig.station, "EVSE 1 charging", |s| {
            s.evses
                .first()
                .and_then(|e| e.transaction.as_ref())
                .is_some_and(|tx| tx.charging_state == ChargingState::Charging)
        })
        .await;
        rig.stop().await;
    })
    .await
    .expect("scenario completes");
}

#[tokio::test]
async fn authorize_resolves_uncached_reservation_group_for_remote_start() {
    tokio::time::timeout(STEP * 4, async {
        let rig = Rig::new().await;
        rig.csms.respond("Authorize", |_| {
            Reply::Result(
                json!({"idTokenInfo": {"status": "Accepted", "groupIdToken": central("G")}}),
            )
        });
        let expiry = rig.in_future(Duration::hours(1));
        let mut body = reserve_body(57, Some(1), "A", &expiry);
        body["groupIdToken"] = central("G");
        assert_eq!(rig.status_of("ReserveNow", body).await, "Accepted");
        rig.send(Command2::PlugIn { evse: 1 }).await;
        let request = json!({
            "remoteStartId": 1,
            "idToken": {"idToken": "MEMBER", "type": "ISO14443"},
            "evseId": 1,
        });
        rig.set_variable("AuthCtrlr", "AuthorizeRemoteStart", "false")
            .await;
        let refused = rig.call("RequestStartTransaction", request.clone()).await;
        assert_eq!(refused["statusInfo"]["reasonCode"], "Reserved");
        assert_eq!(rig.calls("Authorize"), Vec::<Value>::new());
        rig.set_variable("AuthCtrlr", "AuthorizeRemoteStart", "true")
            .await;
        assert_eq!(
            rig.status_of("RequestStartTransaction", request).await,
            "Accepted"
        );
        assert_eq!(rig.wait_event("Started").await["reservationId"], 57);
        assert_eq!(rig.calls("Authorize").len(), 1);
        wait_snapshot(&rig.station, "EVSE 1 charging", |s| {
            s.evses
                .first()
                .and_then(|e| e.transaction.as_ref())
                .is_some_and(|tx| tx.charging_state == ChargingState::Charging)
        })
        .await;
        rig.stop().await;
    })
    .await
    .expect("scenario completes");
}

#[tokio::test]
async fn only_the_reserving_token_starts_on_a_reserved_evse() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(5, Some(1), "A").await, "Accepted");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let refused = rig.present_central(Some(1), "B").await;
    assert_eq!(authorized_status(&refused), "NotAtThisLocation");
    assert_eq!(rig.events_of("Started").len(), 0);
    // The reservation stands.
    let started = rig.present_central(Some(1), "A").await;
    assert!(
        matches!(started, Outcome2::TransactionStarted { .. }),
        "{started:?}"
    );
    // H01.FR.15: the first event names the reservation; it is used up.
    let event = rig.wait_event("Started").await;
    assert_eq!(event["reservationId"], 5);
    assert_eq!(rig.cancel(5).await, "Rejected");
    rig.stop().await;
}

#[tokio::test]
async fn a_token_without_an_evse_goes_to_one_that_is_not_held_for_another() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(1, Some(1), "A").await, "Accepted");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(Command2::PlugIn { evse: 2 }).await;
    let outcome = rig.present_central(None, "B").await;
    match outcome {
        Outcome2::TransactionStarted { evse, .. } => assert_eq!(evse, 2),
        other => panic!("expected a transaction on EVSE 2, got {other:?}"),
    }
    // The reserved EVSE is not offered to anyone else: no EVSE is free for token C.
    let result = rig
        .send_result(Command2::PresentToken {
            evse: None,
            id_token: "C".to_owned(),
            token_type: IdTokenType::Central,
        })
        .await;
    assert!(result.is_err(), "{result:?}");
    rig.stop().await;
}

#[tokio::test]
async fn a_token_of_the_reserved_group_may_use_the_evse() {
    let rig = Rig::new().await;
    rig.csms.respond("Authorize", |payload| {
        let group = if payload["idToken"]["idToken"] == "MEMBER" {
            "G"
        } else {
            "OTHER"
        };
        Reply::Result(json!({"idTokenInfo": {
            "status": "Accepted",
            "groupIdToken": {"idToken": group, "type": "Central"},
        }}))
    });
    let expiry = rig.in_future(Duration::hours(1));
    let mut body = reserve_body(6, Some(1), "A", &expiry);
    body["groupIdToken"] = central("G");
    assert_eq!(rig.status_of("ReserveNow", body).await, "Accepted");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let present = |token: &str| {
        rig.send(Command2::PresentToken {
            evse: Some(1),
            id_token: token.to_owned(),
            token_type: IdTokenType::Iso14443,
        })
    };
    let stranger = present("STRANGER").await;
    assert_eq!(authorized_status(&stranger), "NotAtThisLocation");
    let member = present("MEMBER").await;
    assert!(
        matches!(member, Outcome2::TransactionStarted { .. }),
        "{member:?}"
    );
    assert_eq!(rig.wait_event("Started").await["reservationId"], 6);
    rig.stop().await;
}

#[tokio::test]
async fn a_remote_start_follows_the_reservation() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(8, Some(1), "A").await, "Accepted");
    let body =
        |token: &str, id: i64| json!({"remoteStartId": id, "idToken": central(token), "evseId": 1});
    let reply = rig.call("RequestStartTransaction", body("B", 1)).await;
    assert_eq!(reply["status"], "Rejected");
    assert_eq!(reply["statusInfo"]["reasonCode"], "Reserved");
    let reply = rig.call("RequestStartTransaction", body("A", 2)).await;
    assert_eq!(reply["status"], "Accepted");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    assert_eq!(rig.wait_event("Started").await["reservationId"], 8);
    rig.stop().await;
}

#[tokio::test]
async fn a_reservation_for_any_evse_keeps_one_free_for_its_token() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(9, None, "A").await, "Accepted");
    // H01.FR.07: another token may take an EVSE as long as one stays free.
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let first = rig.present_central(Some(1), "B").await;
    assert!(
        matches!(first, Outcome2::TransactionStarted { .. }),
        "{first:?}"
    );
    // The last free EVSE shows as reserved.
    rig.wait_status(2, "Reserved", 1).await;
    let blocked = rig.present_central(Some(2), "C").await;
    assert_eq!(authorized_status(&blocked), "NotAtThisLocation");
    // The reserving token takes it, and the reservation goes.
    rig.send(Command2::PlugIn { evse: 2 }).await;
    let taken = rig.present_central(Some(2), "A").await;
    assert!(
        matches!(taken, Outcome2::TransactionStarted { .. }),
        "{taken:?}"
    );
    let events = rig.wait_events("Started", 2).await;
    assert!(
        events.iter().any(|event| event["reservationId"] == 9),
        "{events:?}"
    );
    assert_eq!(rig.cancel(9).await, "Rejected");
    rig.stop().await;
}

async fn use_reservation_before_connection(
    protocol: Protocol,
) -> (Rig, broadcast::Receiver<Event>) {
    let rig = if protocol == Protocol::V21 {
        Rig::start_with_mock(v21_csms().await, protocol, "Accepted", |_| {}).await
    } else {
        Rig::new().await
    };
    rig.set_variable("TxCtrlr", "EVConnectionTimeOut", "1")
        .await;
    assert_eq!(rig.reserve(62, Some(1), "A").await, "Accepted");
    let events = rig.station.subscribe();
    assert_eq!(
        authorized_status(&rig.present_central(Some(1), "A").await),
        "Accepted"
    );
    (rig, events)
}

async fn wait_authorization_timeout(events: &mut broadcast::Receiver<Event>, evse: u32) {
    let message = format!("EV did not connect to EVSE {evse} in time: authorization dropped");
    timeout(STEP, async {
        loop {
            if matches!(events.recv().await.expect("station event"), Event::Log(log) if log.text == message)
            {
                break;
            }
        }
    })
    .await
    .expect("authorization expires");
}

async fn assert_reported_without_transaction(rig: &Rig) {
    rig.wait_update(62, "NoTransaction").await;
    assert_eq!(rig.cancel(62).await, "Rejected");
    rig.send(Command2::Heartbeat).await;
    assert_eq!(
        rig.calls("ReservationStatusUpdate"),
        [json!({"reservationId": 62, "reservationUpdateStatus": "NoTransaction"})]
    );
    assert_eq!(rig.events_of("Started"), Vec::<Value>::new());
}

#[tokio::test]
async fn consumed_reservation_connection_timeout_is_reported_in_21() {
    timeout(STEP * 3, async {
        let (rig, _) = use_reservation_before_connection(Protocol::V21).await;
        assert_reported_without_transaction(&rig).await;
        rig.stop().await;
    })
    .await
    .expect("scenario completes");
}

#[tokio::test]
async fn consumed_reservation_connection_timeout_is_reported_after_reconnect() {
    timeout(STEP * 3, async {
        let (rig, mut events) = use_reservation_before_connection(Protocol::V21).await;
        rig.send(Command2::Disconnect).await;
        wait_authorization_timeout(&mut events, 1).await;
        assert_eq!(rig.calls("ReservationStatusUpdate"), Vec::<Value>::new());
        rig.send(Command2::Connect).await;
        assert_reported_without_transaction(&rig).await;
        rig.stop().await;
    })
    .await
    .expect("scenario completes");
}

#[tokio::test]
async fn consumed_reservation_connection_timeout_is_not_reported_in_201() {
    timeout(STEP * 3, async {
        let (rig, mut events) = use_reservation_before_connection(Protocol::V201).await;
        wait_authorization_timeout(&mut events, 1).await;
        assert_eq!(rig.cancel(62).await, "Accepted");
        rig.send(Command2::Heartbeat).await;
        assert_eq!(rig.calls("ReservationStatusUpdate"), Vec::<Value>::new());
        assert_eq!(rig.events_of("Started"), Vec::<Value>::new());
        rig.stop().await;
    })
    .await
    .expect("scenario completes");
}

#[tokio::test]
async fn consumed_reservation_connection_timeout_is_not_reported_after_transaction_start() {
    timeout(STEP * 3, async {
        let (rig, mut events) = use_reservation_before_connection(Protocol::V21).await;
        rig.send(Command2::PlugIn { evse: 1 }).await;
        assert_eq!(rig.wait_event("Started").await["reservationId"], 62);
        // Let another authorization time out after the first one's deadline.
        assert_eq!(
            authorized_status(&rig.present_central(Some(2), "B").await),
            "Accepted"
        );
        wait_authorization_timeout(&mut events, 2).await;
        rig.send(Command2::Heartbeat).await;
        assert_eq!(rig.calls("ReservationStatusUpdate"), Vec::<Value>::new());
        rig.stop().await;
    })
    .await
    .expect("scenario completes");
}

// ---- H04: the end ------------------------------------------------------------------------

#[tokio::test]
async fn an_expired_reservation_is_reported_and_frees_the_evse() {
    let rig = Rig::new().await;
    let expiry = rig.in_future(Duration::milliseconds(200));
    let reply = rig
        .status_of("ReserveNow", reserve_body(3, Some(1), "A", &expiry))
        .await;
    assert_eq!(reply, "Accepted");
    rig.wait_status(1, "Reserved", 1).await;
    // The timer waits for the expiry; the station clock decides whether it has passed.
    rig.clock.advance(Duration::seconds(1));
    rig.wait_update(3, "Expired").await;
    rig.wait_status(1, "Available", 2).await;
    assert_eq!(rig.cancel(3).await, "Rejected");
    rig.stop().await;
}

#[tokio::test]
async fn a_token_is_not_held_back_by_a_reservation_past_its_expiry() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(3, Some(1), "A").await, "Accepted");
    rig.clock.advance(Duration::hours(2));
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let outcome = rig.present_central(Some(1), "B").await;
    assert!(
        matches!(outcome, Outcome2::TransactionStarted { .. }),
        "{outcome:?}"
    );
    rig.wait_update(3, "Expired").await;
    rig.stop().await;
}

#[tokio::test]
async fn a_fault_or_unavailability_removes_the_reservation() {
    let rig = Rig::new().await;
    assert_eq!(rig.reserve(1, Some(1), "A").await, "Accepted");
    assert_eq!(rig.reserve(2, Some(2), "B").await, "Accepted");
    rig.send(Command2::Fault {
        evse: 1,
        connector: None,
    })
    .await;
    rig.wait_update(1, "Removed").await;
    let status = rig
        .status_of(
            "ChangeAvailability",
            json!({"operationalStatus": "Inoperative", "evse": {"id": 2}}),
        )
        .await;
    assert_eq!(status, "Accepted");
    rig.wait_update(2, "Removed").await;
    assert_eq!(rig.cancel(1).await, "Rejected");
    assert_eq!(rig.cancel(2).await, "Rejected");
    // A reservation made again after the fault is cleared holds the EVSE.
    rig.send(Command2::ClearFault {
        evse: 1,
        connector: None,
    })
    .await;
    assert_eq!(rig.reserve(3, Some(1), "A").await, "Accepted");
    rig.stop().await;
}

// ---- persistence -------------------------------------------------------------------------

const ID: &str = "CP-RESERVE";

fn spawn(csms: &MockCsms, dir: &Path, clock: &ManualClock) -> (Station2Handle, JoinHandle<()>) {
    let mut config = StationConfig::new(csms.url())
        .with_charge_point_id(ID)
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(3600)
        .with_reconnect(ReconnectConfig {
            enabled: true,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .with_state_dir(dir);
    config.ws_ping_interval_secs = 0;
    Protocol::V201
        .token()
        .clone_into(&mut config.ws_subprotocol);
    config.protocol = Protocol::V201;
    Station2::spawn(config, Arc::new(clock.clone()))
}

async fn run_once(csms: &MockCsms, dir: &Path, clock: &ManualClock, body: impl AsyncFnOnce()) {
    let (station, join) = spawn(csms, dir, clock);
    station.send(Command2::Connect).await.expect("connect");
    wait_booted(&station).await;
    body().await;
    station.shutdown().await.expect("shutdown");
    timeout(STEP, join).await.expect("stops").expect("joins");
}

async fn call_status(csms: &MockCsms, action: &str, payload: Value) -> String {
    let Frame::CallResult { payload, .. } =
        csms.send_call(action, payload).await.expect("answered")
    else {
        panic!("not a CALLRESULT");
    };
    payload["status"].as_str().expect("a status").to_owned()
}

async fn wait_reserved(csms: &MockCsms, evse: u32, count: usize) {
    csms.wait_for_call_count_after(
        0,
        "StatusNotification",
        count,
        |p| p["evseId"] == evse && p["connectorStatus"] == "Reserved",
        STEP,
    )
    .await
    .expect("the connector reports Reserved");
}

#[tokio::test]
async fn reservations_survive_a_restart() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().expect("dir");
    let clock = ManualClock::new(Utc::now());
    let expiry = stamp(Utc::now() + Duration::hours(1));
    run_once(&csms, dir.path(), &clock, async || {
        let status = call_status(&csms, "ReserveNow", reserve_body(5, Some(1), "A", &expiry)).await;
        assert_eq!(status, "Accepted");
        let status = call_status(&csms, "ReserveNow", reserve_body(6, None, "B", &expiry)).await;
        assert_eq!(status, "Accepted");
        wait_reserved(&csms, 1, 1).await;
    })
    .await;
    run_once(&csms, dir.path(), &clock, async || {
        // The boot reports the restored hold.
        wait_reserved(&csms, 1, 2).await;
        assert_eq!(
            call_status(&csms, "CancelReservation", json!({"reservationId": 5})).await,
            "Accepted"
        );
        assert_eq!(
            call_status(&csms, "CancelReservation", json!({"reservationId": 6})).await,
            "Accepted"
        );
        assert_eq!(
            call_status(&csms, "CancelReservation", json!({"reservationId": 5})).await,
            "Rejected"
        );
    })
    .await;
    csms.shutdown().await;
}

#[tokio::test]
async fn a_cancelled_reservation_stays_cancelled_after_a_restart() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().expect("dir");
    let clock = ManualClock::new(Utc::now());
    let expiry = stamp(Utc::now() + Duration::hours(1));
    run_once(&csms, dir.path(), &clock, async || {
        call_status(&csms, "ReserveNow", reserve_body(5, Some(1), "A", &expiry)).await;
        let status = call_status(&csms, "CancelReservation", json!({"reservationId": 5})).await;
        assert_eq!(status, "Accepted");
    })
    .await;
    run_once(&csms, dir.path(), &clock, async || {
        let status = call_status(&csms, "CancelReservation", json!({"reservationId": 5})).await;
        assert_eq!(status, "Rejected");
    })
    .await;
    csms.shutdown().await;
}

#[tokio::test]
async fn a_restored_list_stays_within_the_evses() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().expect("dir");
    let clock = ManualClock::new(Utc::now());
    let expires_at = Utc::now() + Duration::hours(1);
    let entry = |id: i32, evse: Option<u32>| Reservation {
        id,
        expires_at,
        received_at: None,
        evse,
        connector_type: None,
        id_token: IdTokenRef {
            id_token: format!("T{id}"),
            kind: IdTokenType::Central,
        },
        group_id_token: None,
        lost: false,
    };
    let mut state = PersistedStation2::new(ID);
    state.reservations = ReservationPersist {
        reservations: vec![
            entry(1, Some(1)),
            // A second hold on the same EVSE, one on an EVSE that does not exist and a repeated id.
            entry(2, Some(1)),
            entry(3, Some(7)),
            entry(1, Some(2)),
            // Three for any EVSE on a station with two.
            entry(4, None),
            entry(5, None),
            entry(6, None),
        ],
    };
    StationStore::new_for(FileKind::V2, dir.path(), ID)
        .save2(&state)
        .expect("saved");
    run_once(&csms, dir.path(), &clock, async || {
        for (id, kept) in [
            (1, true),
            (2, false),
            (3, false),
            (4, true),
            (5, true),
            (6, false),
        ] {
            let status =
                call_status(&csms, "CancelReservation", json!({"reservationId": id})).await;
            assert_eq!(status == "Accepted", kept, "reservation {id}: {status}");
        }
    })
    .await;
    csms.shutdown().await;
}

#[tokio::test]
async fn a_state_file_without_reservations_still_loads() {
    let dir = tempfile::tempdir().expect("dir");
    let store = StationStore::new_for(FileKind::V2, dir.path(), ID);
    std::fs::write(
        store.path(),
        format!(r#"{{"version": 1, "charge_point_id": "{ID}"}}"#),
    )
    .expect("write");
    let LoadOutcome2::Loaded(state) = store.load2().expect("loads") else {
        panic!("the file did not load");
    };
    assert_eq!(state.reservations.reservations.len(), 0);
}

// ---- OCPP 2.1 ----------------------------------------------------------------------------

#[tokio::test]
async fn an_ocpp_21_station_reserves_and_reports_the_expiry() {
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.1".to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .expect("mock CSMS starts");
    csms.respond("BootNotification", |_| {
        Reply::Result(json!({
            "status": "Accepted",
            "currentTime": Utc::now().to_rfc3339(),
            "interval": 3600,
        }))
    });
    for action in ["StatusNotification", "ReservationStatusUpdate"] {
        csms.respond(action, |_| Reply::Result(json!({})));
    }
    let clock = ManualClock::new(Utc::now());
    let mut config = StationConfig::new(csms.url())
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(3600)
        .without_persistence();
    config.ws_ping_interval_secs = 0;
    Protocol::V21.token().clone_into(&mut config.ws_subprotocol);
    config.protocol = Protocol::V21;
    let (station, join) = Station2::spawn(config, Arc::new(clock.clone()));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    let expiry = stamp(clock_now(&clock) + Duration::milliseconds(200));
    let status = call_status(&csms, "ReserveNow", reserve_body(1, Some(1), "A", &expiry)).await;
    assert_eq!(status, "Accepted");
    wait_reserved(&csms, 1, 1).await;
    clock.advance(Duration::seconds(1));
    csms.wait_for_call_count_after(
        0,
        "ReservationStatusUpdate",
        1,
        |p| p["reservationId"] == 1 && p["reservationUpdateStatus"] == "Expired",
        STEP,
    )
    .await
    .expect("the expiry is reported");
    station.shutdown().await.expect("shutdown");
    timeout(STEP, join).await.expect("stops").expect("joins");
    csms.shutdown().await;
}

fn clock_now(clock: &ManualClock) -> chrono::DateTime<Utc> {
    use kirby_ocpp_core::Clock;
    clock.now_utc()
}
