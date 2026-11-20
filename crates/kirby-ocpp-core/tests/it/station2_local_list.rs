//! The Local Authorization List of the OCPP 2.x station: `SendLocalList`,
//! `GetLocalListVersion`, the `LocalAuthListCtrlr` variables and authorization from the list
//! (C13, C14, D01, D02).
//!
//! Tests drive the station through the mock CSMS. Waits are bounded by `STEP`; nothing sleeps
//! for a fixed time.

use std::path::Path;
use std::sync::Arc;

use chrono::{Duration, SecondsFormat, Utc};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::ocpp::v2::types::IdTokenType;
use kirby_ocpp_core::store::{FileKind, LoadOutcome2, StationStore};
use kirby_ocpp_core::{Command2, ManualClock, Outcome2, Station2, Station2Handle, StationConfig};
use kirby_ocpp_testkit::{MockCsms, Reply};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::station2_remote::Rig;
use super::station2_support::{STEP, v201_csms, wait_booted};

fn token(id: &str) -> Value {
    json!({"idToken": id, "type": "ISO14443"})
}

/// A list entry with `status`.
fn entry(id: &str, status: &str) -> Value {
    json!({"idToken": token(id), "idTokenInfo": {"status": status}})
}

/// A differential entry that removes `id`.
fn removal(id: &str) -> Value {
    json!({"idToken": token(id)})
}

fn send_list(version: i64, kind: &str, entries: &[Value]) -> Value {
    let mut request = json!({"versionNumber": version, "updateType": kind});
    // The schema wants at least one entry or none at all.
    if !entries.is_empty() {
        request["localAuthorizationList"] = json!(entries);
    }
    request
}

impl Rig {
    async fn send_list(&self, version: i64, kind: &str, entries: &[Value]) -> String {
        self.status_of("SendLocalList", send_list(version, kind, entries))
            .await
    }

    async fn list_version(&self) -> i64 {
        self.call("GetLocalListVersion", json!({})).await["versionNumber"]
            .as_i64()
            .expect("a version")
    }

    /// The `Actual` value of a station variable, read through `GetVariables`.
    async fn variable(&self, component: &str, name: &str) -> String {
        variable_of(&self.csms, component, name).await
    }

    async fn present(&self, id: &str) -> Outcome2 {
        self.present_on(1, id).await
    }

    async fn present_on(&self, evse: u32, id: &str) -> Outcome2 {
        self.send(Command2::PresentToken {
            evse: Some(evse),
            id_token: id.to_owned(),
            token_type: IdTokenType::Iso14443,
        })
        .await
    }
}

async fn variable_of(csms: &MockCsms, component: &str, name: &str) -> String {
    let frame = csms
        .send_call(
            "GetVariables",
            json!({"getVariableData": [{
                "component": {"name": component},
                "variable": {"name": name},
            }]}),
        )
        .await
        .expect("answered");
    let Frame::CallResult { payload, .. } = frame else {
        panic!("not a CALLRESULT: {frame:?}");
    };
    payload["getVariableResult"][0]["attributeValue"]
        .as_str()
        .unwrap_or_else(|| panic!("no value in {payload}"))
        .to_owned()
}

fn bare(id: &str) -> Command2 {
    Command2::Authorize {
        id_token: id.to_owned(),
        token_type: IdTokenType::Iso14443,
    }
}

fn started(outcome: &Outcome2) -> bool {
    matches!(outcome, Outcome2::TransactionStarted { .. })
}

fn status_of(outcome: &Outcome2) -> &str {
    match outcome {
        Outcome2::Authorized { status, .. } => status,
        other => panic!("expected an Authorized outcome, got {other:?}"),
    }
}

/// A station with the list `[LISTED: Accepted]` that is plugged in on EVSE 1.
async fn rig_with_list(entries: &[Value]) -> Rig {
    let rig = Rig::new().await;
    assert_eq!(rig.send_list(1, "Full", entries).await, "Accepted");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig
}

// ---- SendLocalList (D01) -----------------------------------------------------------------

#[tokio::test]
async fn a_full_update_replaces_the_list_and_sets_the_version() {
    let rig = Rig::new().await;
    assert_eq!(
        rig.send_list(1, "Full", &[entry("A", "Accepted"), entry("B", "Accepted")])
            .await,
        "Accepted"
    );
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "2");
    assert_eq!(
        rig.send_list(5, "Full", &[entry("C", "Accepted")]).await,
        "Accepted"
    );
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "1");
    assert_eq!(rig.list_version().await, 5);
    // D01.FR.04: a full update without a list empties it, the version still moves.
    let empty = json!({"versionNumber": 6, "updateType": "Full"});
    assert_eq!(rig.status_of("SendLocalList", empty).await, "Accepted");
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "0");
    assert_eq!(rig.list_version().await, 6);
    rig.stop().await;
}

#[tokio::test]
async fn a_differential_update_adds_replaces_and_removes() {
    let rig = Rig::new().await;
    rig.send_list(1, "Full", &[entry("A", "Accepted"), entry("B", "Accepted")])
        .await;
    let update = [
        entry("C", "Accepted"),
        entry("A", "Blocked"),
        removal("B"),
        removal("NEVER-LISTED"),
    ];
    assert_eq!(rig.send_list(2, "Differential", &update).await, "Accepted");
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "2");
    assert_eq!(rig.list_version().await, 2);
    // What the list says shows in the offline decisions: A is blocked, B is gone, C is in.
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(Command2::Disconnect).await;
    assert_eq!(status_of(&rig.present("A").await), "Unknown");
    assert_eq!(status_of(&rig.present("B").await), "Unknown");
    assert!(started(&rig.present("C").await));
    rig.stop().await;
}

#[tokio::test]
async fn a_differential_update_without_entries_only_moves_the_version() {
    let rig = Rig::new().await;
    rig.send_list(1, "Full", &[entry("A", "Accepted")]).await;
    assert_eq!(rig.send_list(2, "Differential", &[]).await, "Accepted");
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "1");
    assert_eq!(rig.list_version().await, 2);
    rig.stop().await;
}

#[tokio::test]
async fn a_differential_update_with_an_old_version_is_a_version_mismatch() {
    let rig = Rig::new().await;
    rig.send_list(3, "Full", &[entry("A", "Accepted")]).await;
    for version in [3, 2] {
        assert_eq!(
            rig.send_list(version, "Differential", &[entry("B", "Accepted")])
                .await,
            "VersionMismatch"
        );
    }
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "1");
    assert_eq!(rig.list_version().await, 3);
    // D01.FR.19 concerns differential updates: a full one with the same version applies.
    assert_eq!(
        rig.send_list(3, "Full", &[entry("B", "Accepted")]).await,
        "Accepted"
    );
    rig.stop().await;
}

#[tokio::test]
async fn a_version_that_is_not_positive_fails() {
    let rig = Rig::new().await;
    assert_eq!(
        rig.send_list(0, "Full", &[entry("A", "Accepted")]).await,
        "Failed"
    );
    assert_eq!(
        rig.send_list(-4, "Full", &[entry("A", "Accepted")]).await,
        "Failed"
    );
    assert_eq!(rig.list_version().await, 0);
    rig.stop().await;
}

#[tokio::test]
async fn a_full_update_with_an_entry_without_info_fails() {
    let rig = Rig::new().await;
    assert_eq!(
        rig.send_list(1, "Full", &[entry("A", "Accepted"), removal("B")])
            .await,
        "Failed"
    );
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "0");
    rig.stop().await;
}

#[tokio::test]
async fn updates_over_the_caps_fail_and_change_nothing() {
    let rig = Rig::new().await;
    let many = |from: usize, count: usize| -> Vec<Value> {
        (from..from + count)
            .map(|n| entry(&format!("T{n}"), "Accepted"))
            .collect()
    };
    assert_eq!(
        rig.variable("LocalAuthListCtrlr", "ItemsPerMessage").await,
        "1000"
    );
    // More entries than one message may carry.
    assert_eq!(rig.send_list(1, "Full", &many(0, 1001)).await, "Failed");
    assert_eq!(rig.list_version().await, 0);
    // A full list up to the limit is fine.
    assert_eq!(rig.send_list(1, "Full", &many(0, 1000)).await, "Accepted");
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "1000");
    // One more token than the list can hold; the list stays as it was.
    assert_eq!(
        rig.send_list(2, "Differential", &many(1000, 1)).await,
        "Failed"
    );
    assert_eq!(rig.list_version().await, 1);
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "1000");
    // Replacing a listed token does not grow the list.
    assert_eq!(
        rig.send_list(2, "Differential", &many(0, 1)).await,
        "Accepted"
    );
    // A request over BytesPerMessage.
    let mut big = entry("BIG", "Accepted");
    big["customData"] = json!({"vendorId": "acme", "pad": "x".repeat(300_000)});
    assert_eq!(rig.send_list(3, "Differential", &[big]).await, "Failed");
    assert_eq!(rig.list_version().await, 2);
    rig.stop().await;
}

// ---- GetLocalListVersion (D02) -----------------------------------------------------------

#[tokio::test]
async fn the_version_is_zero_before_a_list_and_follows_the_updates() {
    let rig = Rig::new().await;
    assert_eq!(rig.list_version().await, 0);
    rig.send_list(9, "Full", &[entry("A", "Accepted")]).await;
    assert_eq!(rig.list_version().await, 9);
    rig.stop().await;
}

// ---- the variables -----------------------------------------------------------------------

#[tokio::test]
async fn the_variables_describe_the_list() {
    let rig = Rig::new().await;
    let read = |name: &'static str| rig.variable("LocalAuthListCtrlr", name);
    assert_eq!(read("Available").await, "true");
    assert_eq!(read("Enabled").await, "true");
    assert_eq!(read("Entries").await, "0");
    assert_eq!(read("ItemsPerMessage").await, "1000");
    assert_eq!(read("BytesPerMessage").await, "262144");
    assert_eq!(read("SupportsExpiryDateTime").await, "true");
    assert_eq!(read("DisablePostAuthorize").await, "false");
    // `Entries` is read-only.
    let reply = rig
        .call(
            "SetVariables",
            json!({"setVariableData": [{
                "attributeValue": "5",
                "component": {"name": "LocalAuthListCtrlr"},
                "variable": {"name": "Entries"},
            }]}),
        )
        .await;
    assert_eq!(reply["setVariableResult"][0]["attributeStatus"], "Rejected");
    rig.stop().await;
}

// ---- C13: offline ------------------------------------------------------------------------

#[tokio::test]
async fn offline_an_accepted_entry_starts_a_transaction() {
    let rig = rig_with_list(&[entry("LISTED", "Accepted")]).await;
    rig.send(Command2::Disconnect).await;
    let outcome = rig.present("LISTED").await;
    assert!(started(&outcome), "{outcome:?}");
    assert_eq!(rig.calls("Authorize").len(), 0);
    // The transaction reaches the CSMS after the reconnect.
    rig.send(Command2::Connect).await;
    let event = rig.wait_event("Started").await;
    assert_eq!(event["offline"], true);
    assert_eq!(event["idToken"]["idToken"], "LISTED");
    rig.stop().await;
}

#[tokio::test]
async fn offline_an_accepted_entry_needs_local_authorize_offline() {
    let rig = rig_with_list(&[entry("LISTED", "Accepted")]).await;
    rig.set_variable("AuthCtrlr", "LocalAuthorizeOffline", "false")
        .await;
    rig.send(Command2::Disconnect).await;
    assert_eq!(status_of(&rig.present("LISTED").await), "Unknown");
    rig.stop().await;
}

#[tokio::test]
async fn offline_an_entry_that_is_not_accepted_is_refused_even_for_unknown_ids() {
    let rig = rig_with_list(&[entry("BLOCKED", "Blocked")]).await;
    rig.set_variable("AuthCtrlr", "OfflineTxForUnknownIdEnabled", "true")
        .await;
    rig.send(Command2::Disconnect).await;
    // A listed token is not an unknown one.
    assert_eq!(status_of(&rig.present("BLOCKED").await), "Unknown");
    // A token the list does not hold is let in by the setting.
    assert!(started(&rig.present("STRANGER").await));
    rig.stop().await;
}

#[tokio::test]
async fn an_entry_past_its_expiry_reads_as_absent() {
    let past = (Utc::now() - Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let future = (Utc::now() + Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let with_expiry = |id: &str, at: &str| json!({"idToken": token(id), "idTokenInfo": {"status": "Accepted", "cacheExpiryDateTime": at}});
    let rig = rig_with_list(&[with_expiry("OLD", &past), with_expiry("NEW", &future)]).await;
    rig.send(Command2::Disconnect).await;
    assert_eq!(status_of(&rig.present("OLD").await), "Unknown");
    assert!(started(&rig.present("NEW").await));
    rig.stop().await;
}

// ---- C14: online -------------------------------------------------------------------------

#[tokio::test]
async fn online_an_accepted_entry_starts_without_authorize_when_pre_authorizing() {
    let rig = rig_with_list(&[entry("LISTED", "Accepted")]).await;
    rig.set_variable("AuthCtrlr", "LocalPreAuthorize", "true")
        .await;
    let outcome = rig.present("LISTED").await;
    assert!(started(&outcome), "{outcome:?}");
    assert_eq!(rig.calls("Authorize").len(), 0);
    rig.stop().await;
}

#[tokio::test]
async fn online_without_pre_authorize_the_csms_is_asked() {
    let rig = rig_with_list(&[entry("LISTED", "Accepted")]).await;
    let outcome = rig.present("LISTED").await;
    assert!(started(&outcome), "{outcome:?}");
    assert_eq!(rig.calls("Authorize").len(), 1);
    rig.stop().await;
}

#[tokio::test]
async fn online_an_entry_that_is_not_accepted_is_sent_to_the_csms() {
    let rig = rig_with_list(&[entry("BLOCKED", "Blocked")]).await;
    rig.set_variable("AuthCtrlr", "LocalPreAuthorize", "true")
        .await;
    rig.csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo": {"status": "Accepted"}}))
    });
    // C14.FR.03: the CSMS has the last word on an entry other than `Accepted`.
    let outcome = rig.present("BLOCKED").await;
    assert!(started(&outcome), "{outcome:?}");
    assert_eq!(rig.calls("Authorize").len(), 1);
    rig.stop().await;
}

#[tokio::test]
async fn disable_post_authorize_refuses_a_listed_entry_without_asking() {
    let rig = rig_with_list(&[entry("BLOCKED", "Blocked")]).await;
    rig.set_variable("AuthCtrlr", "LocalPreAuthorize", "true")
        .await;
    rig.set_variable("LocalAuthListCtrlr", "DisablePostAuthorize", "true")
        .await;
    assert_eq!(status_of(&rig.present("BLOCKED").await), "Blocked");
    assert_eq!(rig.calls("Authorize").len(), 0);
    rig.stop().await;
}

// ---- the list beats the cache ------------------------------------------------------------

#[tokio::test]
async fn offline_the_list_beats_the_cache() {
    let rig = Rig::new().await;
    // The CSMS accepted CARD once, so the cache holds it as `Accepted`.
    rig.send(bare("CARD")).await;
    rig.send_list(1, "Full", &[entry("CARD", "Blocked")]).await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(Command2::Disconnect).await;
    assert_eq!(status_of(&rig.present("CARD").await), "Unknown");
    rig.stop().await;
}

#[tokio::test]
async fn offline_a_listed_accepted_token_wins_over_a_cached_refusal() {
    let rig = Rig::new().await;
    rig.csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo": {"status": "Blocked"}}))
    });
    rig.send(Command2::PlugIn { evse: 1 }).await;
    assert_eq!(status_of(&rig.present("CARD").await), "Blocked");
    rig.send_list(1, "Full", &[entry("CARD", "Accepted")]).await;
    rig.send(Command2::Disconnect).await;
    assert!(started(&rig.present("CARD").await));
    rig.stop().await;
}

#[tokio::test]
async fn online_a_listed_token_does_not_start_from_the_cache() {
    let rig = Rig::new().await;
    rig.send(bare("CARD")).await;
    assert_eq!(rig.calls("Authorize").len(), 1);
    // Control: a cached `Accepted` token starts without a new `Authorize`.
    rig.send(Command2::PlugIn { evse: 2 }).await;
    assert!(started(&rig.present_on(2, "CARD").await));
    assert_eq!(rig.calls("Authorize").len(), 1);
    // Once the list holds CARD as `Blocked`, the cache entry no longer counts.
    rig.send_list(1, "Full", &[entry("CARD", "Blocked")]).await;
    rig.csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo": {"status": "Blocked"}}))
    });
    rig.send(Command2::PlugIn { evse: 1 }).await;
    assert_eq!(status_of(&rig.present("CARD").await), "Blocked");
    assert_eq!(rig.calls("Authorize").len(), 2);
    rig.stop().await;
}

// ---- Enabled -----------------------------------------------------------------------------

#[tokio::test]
async fn a_disabled_list_is_updated_but_ignored() {
    let rig = Rig::new().await;
    rig.set_variable("LocalAuthListCtrlr", "Enabled", "false")
        .await;
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Enabled").await, "false");
    assert_eq!(
        rig.send_list(4, "Full", &[entry("LISTED", "Accepted")])
            .await,
        "Accepted"
    );
    assert_eq!(rig.variable("LocalAuthListCtrlr", "Entries").await, "1");
    // D02.FR.03: a disabled list reports version 0.
    assert_eq!(rig.list_version().await, 0);
    rig.send(Command2::PlugIn { evse: 1 }).await;
    rig.send(Command2::Disconnect).await;
    assert_eq!(status_of(&rig.present("LISTED").await), "Unknown");
    // Enabled again, the stored list is back in use.
    rig.send(Command2::Connect).await;
    wait_booted(&rig.station).await;
    rig.set_variable("LocalAuthListCtrlr", "Enabled", "true")
        .await;
    assert_eq!(rig.list_version().await, 4);
    rig.send(Command2::Disconnect).await;
    assert!(started(&rig.present("LISTED").await));
    rig.stop().await;
}

// ---- persistence -------------------------------------------------------------------------

const ID: &str = "CP-LIST";

fn spawn(csms: &MockCsms, dir: &Path) -> (Station2Handle, JoinHandle<()>) {
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
    Station2::spawn(config, Arc::new(ManualClock::new(Utc::now())))
}

async fn run_once(csms: &MockCsms, dir: &Path, body: impl AsyncFnOnce(&Station2Handle)) {
    let (station, join) = spawn(csms, dir);
    station.send(Command2::Connect).await.expect("connect");
    wait_booted(&station).await;
    body(&station).await;
    station.shutdown().await.expect("shutdown");
    timeout(STEP, join).await.expect("stops").expect("joins");
}

async fn version_of(csms: &MockCsms) -> i64 {
    let Frame::CallResult { payload, .. } = csms
        .send_call("GetLocalListVersion", json!({}))
        .await
        .expect("answered")
    else {
        panic!("not a CALLRESULT");
    };
    payload["versionNumber"].as_i64().expect("a version")
}

#[tokio::test]
async fn the_list_and_its_version_survive_a_restart() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().expect("dir");
    run_once(&csms, dir.path(), async |_| {
        assert_eq!(version_of(&csms).await, 0);
        let reply = csms
            .send_call(
                "SendLocalList",
                send_list(7, "Full", &[entry("A", "Accepted"), entry("B", "Blocked")]),
            )
            .await
            .expect("answered");
        let Frame::CallResult { payload, .. } = reply else {
            panic!("not a CALLRESULT");
        };
        assert_eq!(payload["status"], "Accepted");
    })
    .await;
    run_once(&csms, dir.path(), async |station| {
        assert_eq!(version_of(&csms).await, 7);
        assert_eq!(
            variable_of(&csms, "LocalAuthListCtrlr", "Entries").await,
            "2"
        );
        // The restored list serves an offline start.
        station
            .send(Command2::PlugIn { evse: 1 })
            .await
            .expect("plug");
        station
            .send(Command2::Disconnect)
            .await
            .expect("disconnect");
        let outcome = station
            .send(Command2::PresentToken {
                evse: Some(1),
                id_token: "A".to_owned(),
                token_type: IdTokenType::Iso14443,
            })
            .await
            .expect("sent");
        assert!(started(&outcome), "{outcome:?}");
    })
    .await;
    csms.shutdown().await;
}

#[tokio::test]
async fn a_state_file_without_a_list_still_loads() {
    let dir = tempfile::tempdir().expect("dir");
    let store = StationStore::new_for(FileKind::V2, dir.path(), ID);
    std::fs::write(
        store.path(),
        format!(r#"{{"version": 1, "charge_point_id": "{ID}", "auth_cache": {{"entries": []}}}}"#),
    )
    .expect("write");
    let LoadOutcome2::Loaded(state) = store.load2().expect("loads") else {
        panic!("the file did not load");
    };
    assert_eq!(state.auth_cache.local_list.version, 0);
    assert!(state.auth_cache.local_list.entries.is_empty());
}
