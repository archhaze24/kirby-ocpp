//! Authorization of the OCPP 2.0.1 station: the `Authorize` exchange, the cache, the offline
//! decision and what the CSMS says about a token during a transaction.
//!
//! Tests that count `Authorize` CALLs ignore what happens to the transaction afterwards and
//! stand on their own; the ones that follow a transaction wait for the transaction engine
//! through snapshots and CALLs. Waits are bounded by `STEP`; nothing sleeps for a fixed time.

use chrono::{Duration, SecondsFormat};
use kirby_ocpp_core::Command2;
use kirby_ocpp_core::Outcome2;
use kirby_ocpp_core::ocpp::v2::types::{IdTokenType, StopReason};
use kirby_ocpp_testkit::Reply;
use serde_json::json;

use super::station2_remote::Rig;
use super::station2_support::wait_snapshot;

fn present(evse: Option<u32>, token: &str, kind: IdTokenType) -> Command2 {
    Command2::PresentToken {
        evse,
        id_token: token.to_owned(),
        token_type: kind,
    }
}

fn card(evse: u32, token: &str) -> Command2 {
    present(Some(evse), token, IdTokenType::Iso14443)
}

fn bare(token: &str) -> Command2 {
    Command2::Authorize {
        id_token: token.to_owned(),
        token_type: IdTokenType::Iso14443,
    }
}

/// The status of an `Authorized` outcome.
fn status(outcome: &Outcome2) -> &str {
    match outcome {
        Outcome2::Authorized { status, .. } => status,
        other => panic!("expected an Authorized outcome, got {other:?}"),
    }
}

/// An `Authorize` answer with `status` and, optionally, a group and an expiry.
fn info(status: &str, group: Option<&str>, expiry: Option<String>) -> Reply {
    let mut info = json!({"status": status});
    if let Some(group) = group {
        info["groupIdToken"] = json!({"idToken": group, "type": "Central"});
    }
    if let Some(expiry) = expiry {
        info["cacheExpiryDateTime"] = json!(expiry);
    }
    Reply::Result(json!({"idTokenInfo": info}))
}

async fn wait_running(rig: &Rig, evse: u32) {
    wait_snapshot(&rig.station, "a transaction", |s| {
        s.evse(evse).is_some_and(|e| e.transaction.is_some())
    })
    .await;
}

async fn wait_idle(rig: &Rig, evse: u32) {
    wait_snapshot(&rig.station, "the transaction ended", |s| {
        s.evse(evse).is_some_and(|e| e.transaction.is_none())
    })
    .await;
}

// ---- the Authorize exchange ------------------------------------------------------------

#[tokio::test]
async fn a_bare_authorize_reports_the_status_of_the_csms() {
    let rig = Rig::new().await;
    let Outcome2::Authorized {
        status: got,
        id_token_info,
    } = rig.send(bare("CARD")).await
    else {
        panic!("expected an Authorized outcome");
    };
    assert_eq!(got, "Accepted");
    assert_eq!(id_token_info, Some(json!({"status": "Accepted"})));
    let sent = rig.calls("Authorize");
    assert_eq!(
        sent,
        [json!({"idToken": {"idToken": "CARD", "type": "ISO14443"}})]
    );
    rig.csms
        .respond("Authorize", |_| info("Blocked", None, None));
    assert_eq!(status(&rig.send(bare("CARD")).await), "Blocked");
    rig.stop().await;
}

#[tokio::test]
async fn a_blocked_token_starts_nothing_and_is_asked_again() {
    let rig = Rig::new().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.csms
        .respond("Authorize", |_| info("Blocked", None, None));
    let outcome = rig.send(card(1, "CARD")).await;
    assert_eq!(status(&outcome), "Blocked");
    assert_eq!(rig.calls("TransactionEvent").len(), 0);
    assert!(
        rig.station
            .snapshot()
            .evse(1)
            .expect("EVSE")
            .transaction
            .is_none()
    );
    // A cached token that is not `Accepted` is checked again (C12.FR.05).
    rig.send(card(1, "CARD")).await;
    assert_eq!(rig.calls("Authorize").len(), 2);
    rig.stop().await;
}

#[tokio::test]
async fn an_accepted_token_starts_a_transaction_with_its_token() {
    let rig = Rig::new().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let outcome = rig.send(card(1, "CARD")).await;
    assert!(
        matches!(outcome, Outcome2::TransactionStarted { evse: 1, .. }),
        "{outcome:?}"
    );
    let started = rig.wait_event("Started").await;
    assert_eq!(
        started["idToken"],
        json!({"idToken": "CARD", "type": "ISO14443"})
    );
    assert_eq!(rig.calls("Authorize").len(), 1);
    rig.stop().await;
}

#[tokio::test]
async fn tokens_that_need_no_check_are_not_sent_to_the_csms() {
    let rig = Rig::new().await;
    rig.csms
        .respond("Authorize", |_| info("Blocked", None, None));
    for (evse, kind) in [(1, IdTokenType::NoAuthorization), (2, IdTokenType::Central)] {
        rig.send(Command2::PlugIn { evse }).await;
        rig.send(present(Some(evse), "FREE", kind)).await;
        wait_running(&rig, evse).await;
    }
    assert_eq!(rig.calls("Authorize").len(), 0);
    rig.stop().await;
}

#[tokio::test]
async fn with_authorization_off_no_authorize_is_sent() {
    let rig = Rig::new().await;
    rig.set_variable("AuthCtrlr", "Enabled", "false").await;
    rig.csms
        .respond("Authorize", |_| info("Blocked", None, None));
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let outcome = rig.send(card(1, "CARD")).await;
    assert!(
        matches!(outcome, Outcome2::TransactionStarted { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.calls("Authorize").len(), 0);
    rig.stop().await;
}

// ---- the cache -------------------------------------------------------------------------

#[tokio::test]
async fn a_cached_token_is_not_asked_again_and_the_event_carries_it() {
    let rig = Rig::new().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(card(1, "CARD")).await;
    wait_running(&rig, 1).await;
    rig.send(Command2::StopTransaction {
        evse: 1,
        reason: StopReason::Local,
    })
    .await;
    wait_idle(&rig, 1).await;
    assert_eq!(rig.calls("Authorize").len(), 1);
    rig.send(card(1, "CARD")).await;
    wait_running(&rig, 1).await;
    // C12.FR.02: no `Authorize`; the CSMS checks the token in the event.
    assert_eq!(rig.calls("Authorize").len(), 1);
    let started = rig.wait_events("Started", 2).await;
    assert_eq!(started[1]["idToken"]["idToken"], "CARD");
    rig.stop().await;
}

#[tokio::test]
async fn a_cache_entry_expires_with_its_date() {
    let rig = Rig::new().await;
    let clock = rig.clock.clone();
    rig.csms.respond("Authorize", move |_| {
        let expiry = (kirby_ocpp_core::Clock::now_utc(&clock) + Duration::seconds(60))
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        info("Accepted", None, Some(expiry))
    });
    rig.send(bare("CARD")).await;
    let _ = rig.send_result(card(1, "CARD")).await;
    assert_eq!(rig.calls("Authorize").len(), 1, "still cached");
    rig.clock.advance(Duration::seconds(120));
    let _ = rig.send_result(card(2, "CARD")).await;
    assert_eq!(rig.calls("Authorize").len(), 2, "expired, asked again");
    rig.stop().await;
}

#[tokio::test]
async fn local_pre_authorize_starts_from_the_cache_and_asks_anyway() {
    let rig = Rig::new().await;
    rig.set_variable("AuthCtrlr", "LocalPreAuthorize", "true")
        .await;
    rig.send(bare("CARD")).await;
    let _ = rig.send_result(card(1, "CARD")).await;
    let asked = rig.wait_calls("Authorize", 2).await;
    assert_eq!(asked.len(), 2);
    rig.stop().await;
}

#[tokio::test]
async fn a_token_of_the_same_group_ends_the_authorization() {
    let rig = Rig::new().await;
    rig.csms
        .respond("Authorize", |_| info("Accepted", Some("FAMILY"), None));
    // The events keep the group, or their answers would replace the cache entry without it.
    rig.csms.respond("TransactionEvent", |payload| {
        if payload.get("idToken").is_some() {
            info("Accepted", Some("FAMILY"), None)
        } else {
            Reply::Result(json!({}))
        }
    });
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(card(1, "PARENT")).await;
    wait_running(&rig, 1).await;
    let outcome = rig.send(card(1, "CHILD")).await;
    assert!(matches!(outcome, Outcome2::Done), "{outcome:?}");
    let ended = rig.wait_event("Ended").await;
    assert_eq!(ended["triggerReason"], "StopAuthorized");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "Local");
    rig.stop().await;
}

#[tokio::test]
async fn a_token_of_the_same_group_ends_the_authorization_without_a_cache() {
    let rig = Rig::new().await;
    rig.set_variable("AuthCacheCtrlr", "Enabled", "false").await;
    rig.csms
        .respond("Authorize", |_| info("Accepted", Some("FAMILY"), None));
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(card(1, "PARENT")).await;
    wait_running(&rig, 1).await;
    let outcome = rig.send(card(1, "CHILD")).await;
    assert!(matches!(outcome, Outcome2::Done), "{outcome:?}");
    let ended = rig.wait_event("Ended").await;
    assert_eq!(ended["triggerReason"], "StopAuthorized");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "Local");
    assert_eq!(ended["idToken"]["idToken"], "CHILD");
    rig.stop().await;
}

#[tokio::test]
async fn a_token_of_the_group_the_event_answer_named_ends_a_remote_start() {
    let rig = Rig::new().await;
    rig.csms
        .respond("Authorize", |_| info("Accepted", Some("FAMILY"), None));
    // The CSMS judges the central token in the answer to the event that carries it.
    rig.csms.respond("TransactionEvent", |payload| {
        if payload.get("idToken").is_some() {
            info("Accepted", Some("FAMILY"), None)
        } else {
            Reply::Result(json!({}))
        }
    });
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let reply = rig
        .call(
            "RequestStartTransaction",
            json!({
                "idToken": {"idToken": "REMOTE", "type": "Central"},
                "remoteStartId": 3,
                "evseId": 1,
            }),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    wait_running(&rig, 1).await;
    rig.wait_event("Started").await;
    // The answer to `Started` reaches the station before the answer to this CALL does.
    rig.call("GetTransactionStatus", json!({})).await;
    let outcome = rig.send(card(1, "CHILD")).await;
    assert!(matches!(outcome, Outcome2::Done), "{outcome:?}");
    let ended = rig.wait_event("Ended").await;
    assert_eq!(ended["triggerReason"], "StopAuthorized");
    assert_eq!(ended["idToken"]["idToken"], "CHILD");
    rig.stop().await;
}

// ---- during a transaction ---------------------------------------------------------------

#[tokio::test]
async fn presenting_the_same_token_again_stops_without_authorize() {
    let rig = Rig::new().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(card(1, "CARD")).await;
    wait_running(&rig, 1).await;
    rig.send(card(1, "CARD")).await;
    let ended = rig.wait_event("Ended").await;
    assert_eq!(ended["triggerReason"], "StopAuthorized");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "Local");
    assert_eq!(rig.calls("Authorize").len(), 1);
    rig.stop().await;
}

#[tokio::test]
async fn an_unrelated_second_token_does_not_take_over() {
    let rig = Rig::new().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(card(1, "FIRST")).await;
    wait_running(&rig, 1).await;
    let outcome = rig.send(card(1, "SECOND")).await;
    assert_eq!(status(&outcome), "ConcurrentTx");
    assert!(
        rig.station
            .snapshot()
            .evse(1)
            .expect("EVSE")
            .transaction
            .is_some()
    );
    assert_eq!(rig.events_of("Ended").len(), 0);
    rig.stop().await;
}

/// Answers the `TransactionEvent` of `token` with `status` in its `idTokenInfo`.
fn judge_events(rig: &Rig, token: &'static str, status: &'static str) {
    rig.csms.respond("TransactionEvent", move |payload| {
        if payload["idToken"]["idToken"] == token {
            Reply::Result(json!({"idTokenInfo": {"status": status}}))
        } else {
            Reply::Result(json!({}))
        }
    });
}

#[tokio::test]
async fn a_token_the_csms_blocks_later_ends_the_transaction() {
    let rig = Rig::new().await;
    judge_events(&rig, "LATE", "Blocked");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(card(1, "LATE")).await;
    let ended = rig.wait_event("Ended").await;
    assert_eq!(ended["triggerReason"], "Deauthorized");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "DeAuthorized");
    wait_idle(&rig, 1).await;
    rig.stop().await;
}

#[tokio::test]
async fn a_later_block_only_stops_the_energy_when_the_transaction_is_to_stay() {
    let rig = Rig::new().await;
    rig.set_variable("TxCtrlr", "StopTxOnInvalidId", "false")
        .await;
    judge_events(&rig, "LATE", "Blocked");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(card(1, "LATE")).await;
    let updated = rig
        .csms
        .wait_for_call_after(
            0,
            "TransactionEvent",
            |p| {
                p["eventType"] == "Updated"
                    && p["transactionInfo"]["chargingState"] == "SuspendedEVSE"
            },
            super::station2_support::STEP,
        )
        .await
        .expect("the energy stops");
    assert_eq!(updated["triggerReason"], "ChargingStateChanged");
    assert!(
        rig.station
            .snapshot()
            .evse(1)
            .expect("EVSE")
            .transaction
            .is_some()
    );
    assert_eq!(rig.events_of("Ended").len(), 0);
    rig.stop().await;
}

#[tokio::test]
async fn a_blocked_answer_is_cached_for_the_next_presentation() {
    let rig = Rig::new().await;
    judge_events(&rig, "LATE", "Blocked");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(card(1, "LATE")).await;
    rig.wait_event("Ended").await;
    wait_idle(&rig, 1).await;
    // Cached as `Blocked`: the station asks the CSMS again, and the answer is `Accepted`.
    rig.csms
        .respond("Authorize", |_| info("Accepted", None, None));
    let before = rig.calls("Authorize").len();
    let _ = rig.send_result(card(1, "LATE")).await;
    assert_eq!(rig.calls("Authorize").len(), before + 1);
    rig.stop().await;
}

// ---- offline ----------------------------------------------------------------------------

#[tokio::test]
async fn offline_an_unknown_token_is_refused_by_default() {
    let rig = Rig::new().await;
    rig.send(Command2::Disconnect).await;
    let outcome = rig.send(card(1, "STRANGER")).await;
    assert_eq!(status(&outcome), "Unknown");
    assert_eq!(rig.calls("Authorize").len(), 0);
    rig.stop().await;
}

#[tokio::test]
async fn offline_a_cached_token_needs_local_authorize_offline() {
    let rig = Rig::new().await;
    rig.send(bare("CARD")).await;
    rig.set_variable("AuthCtrlr", "LocalAuthorizeOffline", "false")
        .await;
    rig.send(Command2::Disconnect).await;
    let outcome = rig.send(card(1, "CARD")).await;
    assert_eq!(status(&outcome), "Unknown");
    rig.stop().await;
}

#[tokio::test]
async fn offline_a_cached_token_starts_a_transaction_that_is_reported_later() {
    let rig = Rig::new().await;
    rig.send(bare("CARD")).await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(Command2::Disconnect).await;
    let outcome = rig.send(card(1, "CARD")).await;
    assert!(
        matches!(outcome, Outcome2::TransactionStarted { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.events_of("Started").len(), 0);
    rig.send(Command2::Connect).await;
    let started = rig.wait_event("Started").await;
    assert_eq!(started["offline"], true);
    assert_eq!(started["idToken"]["idToken"], "CARD");
    assert_eq!(rig.calls("Authorize").len(), 1, "only the bare one");
    rig.stop().await;
}

#[tokio::test]
async fn offline_an_unknown_token_may_start_when_enabled_and_is_not_cached() {
    let rig = Rig::new().await;
    rig.set_variable("AuthCtrlr", "OfflineTxForUnknownIdEnabled", "true")
        .await;
    // An `idTokenInfo` in the answer would cache the token (C10.FR.05); this CSMS sends none.
    rig.csms
        .respond("TransactionEvent", |_| Reply::Result(json!({})));
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(Command2::Disconnect).await;
    let outcome = rig.send(card(1, "STRANGER")).await;
    assert!(
        matches!(outcome, Outcome2::TransactionStarted { .. }),
        "{outcome:?}"
    );
    rig.send(Command2::Connect).await;
    let started = rig.wait_event("Started").await;
    assert_eq!(started["offline"], true);
    assert_eq!(started["idToken"]["idToken"], "STRANGER");
    // C15.FR.01: not added to the cache, so another EVSE has to ask the CSMS.
    let _ = rig.send_result(card(2, "STRANGER")).await;
    assert_eq!(rig.calls("Authorize").len(), 1);
    rig.stop().await;
}

#[tokio::test]
async fn an_unknown_token_that_the_csms_blocks_afterwards_ends_the_offline_transaction() {
    let rig = Rig::new().await;
    rig.set_variable("AuthCtrlr", "OfflineTxForUnknownIdEnabled", "true")
        .await;
    judge_events(&rig, "STRANGER", "Blocked");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(Command2::Disconnect).await;
    rig.send(card(1, "STRANGER")).await;
    rig.send(Command2::Connect).await;
    let ended = rig.wait_event("Ended").await;
    assert_eq!(ended["triggerReason"], "Deauthorized");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "DeAuthorized");
    rig.stop().await;
}
