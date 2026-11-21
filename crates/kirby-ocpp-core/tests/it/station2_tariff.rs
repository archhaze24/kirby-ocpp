//! OCPP 2.1 tariffs and local cost over the station's WebSocket.

use std::path::Path;
use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, IdTokenType};
use kirby_ocpp_core::ocpp::{Protocol, SchemaRegistry};
use kirby_ocpp_core::station::Event;
use kirby_ocpp_core::{
    Clock, Command2, ManualClock, Outcome2, Station2, Station2Handle, SystemClock,
};
use kirby_ocpp_testkit::{ErrorCode, MockCsms, Reply};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::station2_persistence::{config, loaded2, store};
use super::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};
use super::station2_transactions::{call, set_variables, stop};
use super::station21::v21_csms;

const COMPONENT: &str = "TariffCostCtrlr";

async fn boot(csms: &MockCsms, dir: &Path, protocol: Protocol) -> (Station2Handle, JoinHandle<()>) {
    boot_with_clock(csms, dir, protocol, Arc::new(SystemClock)).await
}

async fn boot_with_clock(
    csms: &MockCsms,
    dir: &Path,
    protocol: Protocol,
    clock: Arc<dyn Clock>,
) -> (Station2Handle, JoinHandle<()>) {
    let mut config = config(csms, dir);
    config.protocol = protocol;
    protocol.token().clone_into(&mut config.ws_subprotocol);
    let (station, task) = Station2::spawn(config, clock);
    send(&station, Command2::Connect).await;
    wait_booted(&station).await;
    (station, task)
}

async fn send(station: &Station2Handle, command: Command2) -> Outcome2 {
    timeout(STEP, station.send(command)).await.unwrap().unwrap()
}

async fn enable(csms: &MockCsms, running: bool) {
    let settings = [
        (COMPONENT, "Enabled", Some("Tariff"), "true"),
        (COMPONENT, "Enabled", Some("Cost"), "true"),
        (COMPONENT, "Enabled", Some("RunningCost"), "true"),
        (COMPONENT, "Interval", Some("Cost"), "1"),
    ];
    set_variables(csms, if running { &settings } else { &settings[..2] }).await;
}

fn energy(id: &str, price: f64) -> Value {
    json!({"tariffId":id, "currency":"USD","energy":{"prices":[{"priceKwh":price}]}})
}

async fn install(csms: &MockCsms, evse: u32, tariff: Value) -> Value {
    call(
        csms,
        "SetDefaultTariff",
        json!({"evseId":evse,"tariff":tariff}),
    )
    .await
}

async fn get(csms: &MockCsms, evse: u32) -> Value {
    call(csms, "GetTariffs", json!({"evseId":evse})).await
}

async fn event(csms: &MockCsms, mark: usize, field: &str, value: &str) -> Value {
    csms.wait_for_call_after(mark, "TransactionEvent", |p| p[field] == value, STEP)
        .await
        .unwrap_or_else(|err| panic!("TransactionEvent {field} {value}: {err}"))
}

async fn present(station: &Station2Handle, token: &str, kind: IdTokenType) -> Outcome2 {
    send(
        station,
        Command2::PresentToken {
            evse: Some(1),
            id_token: token.to_owned(),
            token_type: kind,
        },
    )
    .await
}

async fn start(csms: &MockCsms, station: &Station2Handle) -> Value {
    let mark = csms.mark();
    send(station, Command2::PlugIn { evse: 1 }).await;
    present(station, "TAG", IdTokenType::Central).await;
    event(csms, mark, "eventType", "Started").await
}

async fn end(csms: &MockCsms, station: &Station2Handle) -> Value {
    let mark = csms.mark();
    send(station, Command2::Unplug { evse: 1 }).await;
    event(csms, mark, "eventType", "Ended").await
}

async fn advance(station: &Station2Handle, wh_delta: u64) {
    send(station, Command2::AdvanceMeter { evse: 1, wh_delta }).await;
}

fn near(actual: &Value, expected: f64, tolerance: f64) {
    let actual = actual.as_f64().expect("numeric field");
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected}"
    );
}

fn variable(name: &str, instance: Option<&str>) -> Value {
    let mut result = json!({"name":name});
    if let Some(instance) = instance {
        result["instance"] = json!(instance);
    }
    result
}

#[tokio::test]
async fn tariff_device_model_and_messages_are_2_1_only() {
    let rows = [
        ("Available", Some("Tariff"), "true"),
        ("Enabled", Some("Tariff"), "false"),
        ("Available", Some("Cost"), "true"),
        ("Enabled", Some("Cost"), "false"),
        ("Enabled", Some("RunningCost"), "false"),
        ("Interval", Some("Cost"), "60"),
        ("Interval", Some("Tariff"), "0"),
        ("MaxElements", Some("Tariff"), "10"),
        ("ConditionsSupported", Some("Tariff"), "false"),
        ("HandleFailedTariff", Some("Tariff"), "Deauthorize"),
        ("TariffFallbackMessage", None, ""),
        ("TotalCostFallbackMessage", None, ""),
        ("Problem", None, "false"),
    ];
    for protocol in [Protocol::V21, Protocol::V201] {
        let (csms, code) = if protocol == Protocol::V21 {
            (v21_csms().await, ErrorCode::NotSupported)
        } else {
            (v201_csms().await, ErrorCode::NotImplemented)
        };
        let dir = tempfile::tempdir().unwrap();
        let (station, task) = boot(&csms, dir.path(), protocol).await;
        for (name, instance, expected) in rows {
            let result = call(
                &csms,
                "GetVariables",
                json!({"getVariableData":[{
                    "component":{"name":COMPONENT},"variable":variable(name, instance)
                }]}),
            )
            .await;
            let row = &result["getVariableResult"][0];
            if protocol == Protocol::V21 {
                assert_eq!(row["attributeStatus"], "Accepted");
                assert_eq!(row["attributeValue"], expected, "{name} {instance:?}");
            } else {
                assert_eq!(row["attributeStatus"], "UnknownComponent");
            }
        }
        for (action, payload) in [
            (
                "SetDefaultTariff",
                json!({"evseId":1,"tariff":energy("A", 1.0)}),
            ),
            ("GetTariffs", json!({"evseId":0})),
            ("ClearTariffs", json!({})),
            (
                "ChangeTransactionTariff",
                json!({"transactionId":"missing","tariff":energy("B", 1.0)}),
            ),
        ] {
            csms.expect_call_error(action, payload, &code)
                .await
                .unwrap();
        }
        if protocol == Protocol::V21 {
            let result = call(
                &csms,
                "SetVariables",
                json!({"setVariableData":[{
                    "component":{"name":COMPONENT},"variable":variable("Interval",Some("Cost")),
                    "attributeValue":"0"
                }]}),
            )
            .await;
            assert_eq!(
                result["setVariableResult"][0]["attributeStatus"],
                "Rejected"
            );
        }
        stop(&station, task).await;
    }
}

#[tokio::test]
async fn tariff_defaults_validate_override_and_clear() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    assert_eq!(get(&csms, 0).await, json!({"status":"NoTariff"}));
    assert_eq!(
        get(&csms, 3).await["statusInfo"]["reasonCode"],
        "UnknownEVSE"
    );
    let unknown = install(&csms, 3, energy("A", -1.0)).await;
    assert_eq!(unknown["status"], "Rejected");
    assert_eq!(unknown["statusInfo"]["reasonCode"], "UnknownEVSE");
    for tariff in [energy("A", -0.01), json!({"tariffId":"A","currency":"USD"})] {
        let invalid = install(&csms, 0, tariff).await;
        assert_eq!(invalid["status"], "Rejected");
        assert_eq!(invalid["statusInfo"]["reasonCode"], "InvalidValue");
    }
    let mut many = energy("A", 0.25);
    many["energy"]["prices"] = json!(vec![json!({"priceKwh":0.25,"conditions":{}}); 11]);
    assert_eq!(install(&csms, 0, many).await["status"], "TooManyElements");
    let mut conditional = energy("A", 0.25);
    conditional["energy"]["prices"][0]["conditions"] = json!({});
    assert_eq!(
        install(&csms, 0, conditional).await["status"],
        "ConditionNotSupported"
    );
    assert_eq!(
        install(&csms, 0, energy("A", 1.0)).await["status"],
        "Accepted"
    );
    assert_eq!(
        install(&csms, 2, energy("A", 1.0)).await["status"],
        "DuplicateTariffId"
    );
    assert_eq!(
        get(&csms, 0).await["tariffAssignments"][0]["evseIds"],
        json!([1, 2])
    );
    assert_eq!(
        install(&csms, 2, energy("B", 1.0)).await["status"],
        "Accepted"
    );
    assert_eq!(
        get(&csms, 0).await["tariffAssignments"],
        json!([
            {"tariffId":"A","tariffKind":"DefaultTariff","evseIds":[1]},
            {"tariffId":"B","tariffKind":"DefaultTariff","evseIds":[2]}
        ])
    );
    for i in 0..5 {
        let mut future = energy(&format!("future-{i}"), 0.25);
        future["validFrom"] = json!(Utc::now() + TimeDelta::hours(1));
        let result = install(&csms, 2, future).await;
        if i < 4 {
            assert_eq!(result["status"], "Accepted");
        } else {
            assert_eq!(result["status"], "Rejected");
            assert_eq!(result["statusInfo"]["reasonCode"], "OutOfMemory");
        }
    }
    for (request, expected) in [
        (
            json!({"evseId":3}),
            json!([{"status":"Rejected","statusInfo":{"reasonCode":"UnknownEVSE"}}]),
        ),
        (
            json!({"evseId":2,"tariffIds":["A"]}),
            json!([{"status":"NoTariff"}]),
        ),
        (
            json!({"evseId":1,"tariffIds":["A","B"]}),
            json!([{"tariffId":"A","status":"Accepted"},{"tariffId":"B","status":"NoTariff"}]),
        ),
    ] {
        assert_eq!(
            call(&csms, "ClearTariffs", request).await["clearTariffsResult"],
            expected
        );
    }
    assert_eq!(get(&csms, 1).await["status"], "NoTariff");
    assert_eq!(
        get(&csms, 2).await["tariffAssignments"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        call(&csms, "ClearTariffs", json!({"evseId":0})).await["clearTariffsResult"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(get(&csms, 0).await["status"], "NoTariff");
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_future_activation_and_default_persistence() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    install(&csms, 0, energy("base", 1.0)).await;
    let mut future = energy("future", 2.0);
    future["validFrom"] = json!(Utc::now() + TimeDelta::seconds(2));
    install(&csms, 1, future.clone()).await;
    assert_eq!(
        get(&csms, 1).await["tariffAssignments"][1]["validFrom"],
        future["validFrom"]
    );
    assert_eq!(
        station
            .snapshot()
            .evse(1)
            .unwrap()
            .default_tariff_id
            .as_deref(),
        Some("base")
    );
    wait_snapshot(&station, "tariff activation", |s| {
        s.evse(1).unwrap().default_tariff_id.as_deref() == Some("future")
    })
    .await;
    assert_eq!(
        get(&csms, 1).await["tariffAssignments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    stop(&station, task).await;
    let store = store(dir.path());
    let mut persisted = loaded2(store.load2().unwrap());
    persisted.default_tariffs.extend([
        json!({"evseId":2,"tariff":energy("invalid", -1.0),"validFrom":null}),
        json!({"evseId":2,"tariff":energy("broken", 1.0),"validFrom":"unknown"}),
    ]);
    store.save2(&persisted).unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    assert_eq!(
        get(&csms, 1).await["tariffAssignments"][0]["tariffId"],
        "future"
    );
    assert_eq!(
        get(&csms, 2).await["tariffAssignments"][0]["tariffId"],
        "base"
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_driver_cache_assignment_and_transaction_cleanup() {
    let csms = v21_csms().await;
    csms.respond("Authorize", |_| {
        Reply::Result(json!({
            "idTokenInfo":{"status":"Accepted"},"tariff":energy("driver", 2.0)
        }))
    });
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    send(
        &station,
        Command2::Authorize {
            id_token: "driver-tag".to_owned(),
            token_type: IdTokenType::Local,
        },
    )
    .await;
    assert_eq!(
        get(&csms, 0).await["tariffAssignments"],
        json!([
            {"tariffId":"driver","tariffKind":"DriverTariff","idTokens":["driver-tag"]}
        ])
    );
    assert_eq!(get(&csms, 1).await["status"], "NoTariff");
    assert_eq!(
        install(&csms, 1, energy("driver", 1.0)).await["status"],
        "DuplicateTariffId"
    );
    let mark = csms.mark();
    send(&station, Command2::PlugIn { evse: 1 }).await;
    present(&station, "driver-tag", IdTokenType::Local).await;
    let started = event(&csms, mark, "eventType", "Started").await;
    assert_eq!(started["transactionInfo"]["tariffId"], "driver");
    assert!(
        csms.calls_since(mark)
            .iter()
            .all(|entry| entry.action != "Authorize")
    );
    let assigned = get(&csms, 1).await;
    assert_eq!(
        assigned["tariffAssignments"],
        json!([
            {"tariffId":"driver","tariffKind":"DriverTariff","idTokens":["driver-tag"],"evseIds":[1]}
        ])
    );
    assert_eq!(
        call(&csms, "ClearTariffs", json!({})).await,
        json!({"clearTariffsResult":[{"status":"NoTariff"}]})
    );
    end(&csms, &station).await;
    assert_eq!(get(&csms, 0).await["status"], "NoTariff");
    // Ending drops the live assignment. The authorization cache can still recreate it.
    let mark = csms.mark();
    send(&station, Command2::PlugIn { evse: 1 }).await;
    present(&station, "driver-tag", IdTokenType::Local).await;
    assert_eq!(
        event(&csms, mark, "eventType", "Started").await["transactionInfo"]["tariffId"],
        "driver"
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_driver_cleanup_after_ev_connection_timeout() {
    let csms = v21_csms().await;
    csms.respond("Authorize", |_| {
        Reply::Result(json!({
            "idTokenInfo":{"status":"Accepted"},"tariff":energy("driver", 2.0)
        }))
    });
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    set_variables(&csms, &[("TxCtrlr", "EVConnectionTimeOut", None, "1")]).await;
    let mut events = station.subscribe();
    present(&station, "waiting-tag", IdTokenType::Local).await;
    assert_eq!(
        get(&csms, 0).await["tariffAssignments"][0]["tariffId"],
        "driver"
    );
    timeout(STEP, async {
        loop {
            if matches!(events.recv().await.unwrap(), Event::Log(entry) if entry.text.contains("authorization dropped")) {
                break;
            }
        }
    }).await.unwrap();
    assert_eq!(get(&csms, 0).await["status"], "NoTariff");
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_remote_start_associates_driver_tariff_at_later_authorization() {
    let csms = v21_csms().await;
    csms.respond("Authorize", |_| {
        Reply::Result(json!({
            "idTokenInfo":{"status":"Accepted"},"tariff":energy("remote-driver", 2.0)
        }))
    });
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    set_variables(&csms, &[("TxCtrlr", "TxStartPoint", None, "EVConnected")]).await;
    let mark = csms.mark();
    send(&station, Command2::PlugIn { evse: 1 }).await;
    let started = event(&csms, mark, "eventType", "Started").await;
    assert!(started["transactionInfo"].get("tariffId").is_none());
    assert_eq!(
        call(
            &csms,
            "RequestStartTransaction",
            json!({
                "evseId":1,"remoteStartId":77,"idToken":{"idToken":"remote","type":"Local"}
            })
        )
        .await["status"],
        "Accepted"
    );
    let updated = event(&csms, mark, "triggerReason", "RemoteStart").await;
    assert_eq!(updated["transactionInfo"]["tariffId"], "remote-driver");
    assert_eq!(
        updated["transactionInfo"]["transactionId"],
        started["transactionInfo"]["transactionId"]
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_invalid_driver_fallback_and_problem_notifications() {
    for (policy, tariff_id) in [
        ("Deauthorize", None),
        ("UseDefaultTariff", Some("default")),
        ("CentralCost", None),
    ] {
        let csms = v21_csms().await;
        csms.respond("NotifyEvent", |_| Reply::Result(json!({})));
        csms.respond("Authorize", |request| {
            let mut tariff = energy("driver", 1.0);
            if request["idToken"]["idToken"] == "bad" {
                tariff["energy"]["prices"][0]["conditions"] = json!({});
            }
            Reply::Result(json!({"idTokenInfo":{"status":"Accepted"},"tariff":tariff}))
        });
        let dir = tempfile::tempdir().unwrap();
        let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
        enable(&csms, false).await;
        set_variables(
            &csms,
            &[(COMPONENT, "HandleFailedTariff", Some("Tariff"), policy)],
        )
        .await;
        install(&csms, 1, energy("default", 1.0)).await;
        let mark = csms.mark();
        send(&station, Command2::PlugIn { evse: 1 }).await;
        let authorized = present(&station, "bad", IdTokenType::Local).await;
        let raised = csms
            .wait_for_call_after(mark, "NotifyEvent", |_| true, STEP)
            .await
            .unwrap();
        let problem = &raised["eventData"][0];
        assert_eq!(raised["seqNo"], 0);
        assert_eq!(problem["component"]["name"], COMPONENT);
        assert_eq!(problem["variable"]["name"], "Problem");
        assert_eq!(problem["actualValue"], "true");
        assert_eq!(problem["techCode"], "ConditionNotSupported");
        let result = call(
            &csms,
            "GetVariables",
            json!({"getVariableData":[{
                "component":{"name":COMPONENT},"variable":{"name":"Problem"}
            }]}),
        )
        .await;
        assert_eq!(result["getVariableResult"][0]["attributeValue"], "true");
        if policy == "Deauthorize" {
            assert!(matches!(authorized, Outcome2::Authorized {status, ..} if status == "Invalid"));
            assert!(station.snapshot().evse(1).unwrap().transaction.is_none());
        } else {
            assert_eq!(
                event(&csms, mark, "eventType", "Started").await["transactionInfo"]["tariffId"]
                    .as_str(),
                tariff_id
            );
            let ended = end(&csms, &station).await;
            assert_eq!(
                ended.get("costDetails").is_some(),
                policy == "UseDefaultTariff"
            );
        }
        send(
            &station,
            Command2::Authorize {
                id_token: "good".to_owned(),
                token_type: IdTokenType::Local,
            },
        )
        .await;
        let cleared = csms
            .wait_for_call_after(
                mark,
                "NotifyEvent",
                |p| p["eventData"][0]["actualValue"] == "false",
                STEP,
            )
            .await
            .unwrap();
        let cleared = &cleared["eventData"][0];
        assert!(cleared["eventId"].as_u64() > problem["eventId"].as_u64());
        assert!(cleared.get("techCode").is_none());
        assert_eq!(
            csms.calls_since(mark)
                .iter()
                .filter(|p| p.action == "NotifyEvent")
                .count(),
            2
        );
        stop(&station, task).await;
    }
}

#[tokio::test]
async fn tariff_running_and_ended_cost_details() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, true).await;
    let mut tariff = energy("energy", 0.25);
    tariff["energy"]["taxRates"] = json!([{"type":"federal","tax":6},{"type":"state","tax":4}]);
    install(&csms, 1, tariff).await;
    let mark = csms.mark();
    let started = start(&csms, &station).await;
    assert_eq!(started["transactionInfo"]["tariffId"], "energy");
    assert!(started["costDetails"].is_object());
    assert!(started["costDetails"].get("chargingPeriods").is_none());
    advance(&station, 1000).await;
    let first = event(&csms, mark, "triggerReason", "RunningCost").await;
    let seq_no = first["seqNo"].as_u64().unwrap();
    let second = csms
        .wait_for_call_after(
            mark,
            "TransactionEvent",
            |p| p["triggerReason"] == "RunningCost" && p["seqNo"].as_u64().unwrap() > seq_no,
            STEP,
        )
        .await
        .unwrap();
    for update in [&first, &second] {
        assert!(update["costDetails"].get("chargingPeriods").is_none());
    }
    assert!(
        second["costDetails"]["totalUsage"]["energy"].as_f64()
            >= first["costDetails"]["totalUsage"]["energy"].as_f64()
    );
    call(&csms, "ClearTariffs", json!({})).await;
    assert_eq!(get(&csms, 1).await["status"], "NoTariff");
    assert_eq!(
        install(&csms, 2, energy("energy", 1.0)).await["status"],
        "DuplicateTariffId"
    );
    let ended = end(&csms, &station).await;
    let details = &ended["costDetails"];
    let periods = details["chargingPeriods"].as_array().unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0]["tariffId"], "energy");
    let volume = &details["totalUsage"]["energy"];
    assert_eq!(
        periods[0]["dimensions"],
        json!([{"type":"Energy","volume":volume}])
    );
    let excl = volume.as_f64().unwrap() / 1000.0 * 0.25;
    let total = &details["totalCost"];
    near(&total["energy"]["exclTax"], excl, 0.0001);
    near(&total["energy"]["inclTax"], excl * 1.10, 0.0001);
    assert_eq!(total["currency"], "USD");
    assert_eq!(total["typeOfCost"], "NormalCost");
    let events = csms.calls_since(mark);
    assert_eq!(
        events
            .iter()
            .filter(|entry| entry.action == "TransactionEvent"
                && entry.payload["transactionInfo"].get("tariffId").is_some())
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .filter(|entry| entry.action == "TransactionEvent"
                && entry.payload["eventType"] == "Updated"
                && entry.payload["triggerReason"] != "RunningCost")
            .all(|entry| entry.payload.get("costDetails").is_none())
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_charging_idle_and_reservation_costs() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(Utc::now());
    let (station, task) =
        boot_with_clock(&csms, dir.path(), Protocol::V21, Arc::new(clock.clone())).await;
    enable(&csms, true).await;
    let tariff = json!({
        "tariffId":"time","currency":"USD",
        "chargingTime":{"prices":[{"priceMinute":0.60}]},
        "idleTime":{"prices":[{"priceMinute":1.20}]},
        "reservationFixed":{"prices":[{"priceFixed":1.0}]},
        "reservationTime":{"prices":[{"priceMinute":0.5}]}
    });
    install(&csms, 1, tariff).await;
    assert_eq!(
        call(
            &csms,
            "ReserveNow",
            json!({
                "id":1,"evseId":1, "expiryDateTime": Utc::now() + TimeDelta::minutes(5),
                "idToken":{"idToken":"TAG","type":"Central"}
            })
        )
        .await["status"],
        "Accepted"
    );
    clock.advance(TimeDelta::minutes(2));
    let mark = csms.mark();
    assert_eq!(start(&csms, &station).await["reservationId"], 1);
    csms.wait_for_call_after(
        mark,
        "TransactionEvent",
        |p| {
            p["triggerReason"] == "RunningCost"
                && p["costDetails"]["totalUsage"]["chargingTime"]
                    .as_u64()
                    .unwrap_or_default()
                    > 0
        },
        STEP,
    )
    .await
    .unwrap();
    send(
        &station,
        Command2::SetChargingState {
            evse: 1,
            state: ChargingState::SuspendedEv,
        },
    )
    .await;
    csms.wait_for_call_after(
        mark,
        "TransactionEvent",
        |p| {
            p["triggerReason"] == "RunningCost"
                && p["costDetails"]["totalUsage"]["idleTime"]
                    .as_u64()
                    .unwrap_or_default()
                    > 0
        },
        STEP,
    )
    .await
    .unwrap();
    let ended = end(&csms, &station).await;
    let details = &ended["costDetails"];
    let usage = &details["totalUsage"];
    let costs = &details["totalCost"];
    assert_eq!(
        details["chargingPeriods"][0]["dimensions"],
        json!([
            {"type":"ChargingTime","volume":usage["chargingTime"]},
            {"type":"IdleTIme","volume":usage["idleTime"]}
        ])
    );
    near(
        &costs["chargingTime"]["exclTax"],
        usage["chargingTime"].as_f64().unwrap() * 0.01,
        0.0001,
    );
    near(
        &costs["idleTime"]["exclTax"],
        usage["idleTime"].as_f64().unwrap() * 0.02,
        0.0001,
    );
    assert_eq!(costs["reservationFixed"]["exclTax"], 1.0);
    assert_eq!(costs["reservationTime"]["exclTax"], 1.0);
    assert_eq!(usage["reservationTime"], 120);
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_cost_limit_uses_local_cost_and_ignores_csms_updates() {
    let csms = v21_csms().await;
    csms.respond("TransactionEvent", |p| {
        Reply::Result(if p["eventType"] == "Started" {
            json!({"totalCost":0,"transactionLimit":{"maxCost":0.01}})
        } else {
            json!({"totalCost":0})
        })
    });
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    install(&csms, 1, energy("local", 0.25)).await;
    let mark = csms.mark();
    let started = start(&csms, &station).await;
    event(&csms, mark, "triggerReason", "LimitSet").await;
    assert_eq!(
        call(
            &csms,
            "CostUpdated",
            json!({
                "transactionId":started["transactionInfo"]["transactionId"],"totalCost":1000
            })
        )
        .await,
        json!({})
    );
    assert!(station.snapshot().evse(1).unwrap().total_cost.unwrap() < 0.01);
    advance(&station, 100).await;
    let reached = event(&csms, mark, "triggerReason", "CostLimitReached").await;
    assert_eq!(reached["transactionInfo"]["chargingState"], "SuspendedEVSE");
    assert!(station.snapshot().evse(1).unwrap().total_cost.unwrap() >= 0.01);
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_cost_state_survives_e17_resume() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    set_variables(&csms, &[("TxCtrlr", "TxResumptionTimeout", None, "60")]).await;
    install(&csms, 1, energy("energy", 1.0)).await;
    start(&csms, &station).await;
    advance(&station, 1000).await;
    stop(&station, task).await;
    let mark = csms.mark();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    let resumed = event(&csms, mark, "triggerReason", "TxResumed").await;
    assert!(resumed["transactionInfo"].get("tariffId").is_none());
    assert_eq!(
        station.snapshot().evse(1).unwrap().tariff_id.as_deref(),
        Some("energy")
    );
    advance(&station, 1000).await;
    let ended = end(&csms, &station).await;
    assert!(
        ended["costDetails"]["totalUsage"]["energy"]
            .as_f64()
            .unwrap()
            >= 2000.0
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_cost_state_survives_b13_resume() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    set_variables(&csms, &[("TxCtrlr", "TxResumptionTimeout", None, "60")]).await;
    install(&csms, 1, energy("energy", 1.0)).await;
    let started = start(&csms, &station).await;
    advance(&station, 1000).await;
    let mark = csms.mark();
    assert_eq!(
        call(&csms, "Reset", json!({"type":"ImmediateAndResume"})).await["status"],
        "Accepted"
    );
    let resumed = event(&csms, mark, "triggerReason", "TxResumed").await;
    assert!(resumed["transactionInfo"].get("tariffId").is_none());
    assert_eq!(
        resumed["transactionInfo"]["transactionId"],
        started["transactionInfo"]["transactionId"]
    );
    advance(&station, 1000).await;
    let ended = end(&csms, &station).await;
    assert!(
        ended["costDetails"]["totalUsage"]["energy"]
            .as_f64()
            .unwrap()
            >= 2000.0
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_change_sets_a_new_period_and_rejects_bad_targets() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    install(&csms, 1, energy("first", 1.0)).await;
    let change = |id: Value, tariff: Value| {
        call(
            &csms,
            "ChangeTransactionTariff",
            json!({"transactionId":id,"tariff":tariff}),
        )
    };
    assert_eq!(
        change(json!("missing"), energy("second", 2.0)).await["status"],
        "TxNotFound"
    );
    let started = start(&csms, &station).await;
    let id = &started["transactionInfo"]["transactionId"];
    let mut other_currency = energy("second", 2.0);
    other_currency["currency"] = json!("EUR");
    assert_eq!(
        change(id.clone(), other_currency).await["status"],
        "NoCurrencyChange"
    );
    advance(&station, 1000).await;
    let mark = csms.mark();
    assert_eq!(
        change(id.clone(), energy("second", 2.0)).await["status"],
        "Accepted"
    );
    let updated = event(&csms, mark, "triggerReason", "TariffChanged").await;
    assert_eq!(updated["transactionInfo"]["tariffId"], "second");
    let assignments = get(&csms, 1).await;
    assert_eq!(
        assignments["tariffAssignments"][1],
        json!({"tariffId":"second","tariffKind":"DriverTariff","evseIds":[1]})
    );
    advance(&station, 1000).await;
    let ended = end(&csms, &station).await;
    let periods = ended["costDetails"]["chargingPeriods"].as_array().unwrap();
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[0]["tariffId"], "first");
    assert_eq!(periods[1]["tariffId"], "second");
    let expected = (periods[0]["dimensions"][0]["volume"].as_f64().unwrap()
        + 2.0 * periods[1]["dimensions"][0]["volume"].as_f64().unwrap())
        / 1000.0;
    near(
        &ended["costDetails"]["totalCost"]["energy"]["exclTax"],
        expected,
        0.0001,
    );
    assert_eq!(
        get(&csms, 1).await["tariffAssignments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_review_rejects_oversized_payloads() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    let mut tariff = energy("oversized", 1.0);
    tariff["description"] = json!(vec![
        json!({
            "format":"UTF8","content":"🟢".repeat(1024)
        });
        5
    ]);
    assert_eq!(
        install(&csms, 1, tariff.clone()).await["status"],
        "TooManyElements"
    );
    assert_eq!(get(&csms, 0).await, json!({"status":"NoTariff"}));

    let started = start(&csms, &station).await;
    assert_eq!(
        call(
            &csms,
            "ChangeTransactionTariff",
            json!({"transactionId":started["transactionInfo"]["transactionId"],"tariff":tariff})
        )
        .await["status"],
        "TooManyElements"
    );
    end(&csms, &station).await;

    csms.respond("Authorize", move |_| {
        Reply::Result(json!({"idTokenInfo":{"status":"Accepted"},"tariff":tariff}))
    });
    let mark = csms.mark();
    let result = send(
        &station,
        Command2::Authorize {
            id_token: "oversized".to_owned(),
            token_type: IdTokenType::Local,
        },
    )
    .await;
    assert!(matches!(result, Outcome2::Authorized { status, .. } if status == "Invalid"));
    let raised = csms
        .wait_for_call_after(mark, "NotifyEvent", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(raised["eventData"][0]["techCode"], "TooManyElements");
    stop(&station, task).await;
    let state = loaded2(store(dir.path()).load2().unwrap());
    assert_eq!(state.auth_cache.entries.len(), 0);
}

#[tokio::test]
async fn tariff_review_ordinary_settle_caps_billed_energy() {
    let csms = v21_csms().await;
    csms.respond("TransactionEvent", |request| {
        Reply::Result(if request["eventType"] == "Started" {
            json!({"transactionLimit":{"maxEnergy":1.0}})
        } else {
            json!({})
        })
    });
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    let mut tariff = energy("energy", 1.0);
    tariff["maxCost"] = json!({"exclTax":0.002});
    install(&csms, 1, tariff).await;
    let mark = csms.mark();
    start(&csms, &station).await;
    event(&csms, mark, "triggerReason", "LimitSet").await;

    timeout(STEP, async {
        loop {
            let mark = csms.mark();
            send(&station, Command2::SampleMeter { evse: 1 }).await;
            event(&csms, mark, "triggerReason", "MeterValuePeriodic").await;
            if station.snapshot().evse(1).unwrap().meter_wh >= 1 {
                break;
            }
        }
    })
    .await
    .expect("ordinary meter samples reach the energy cap");
    let ended = end(&csms, &station).await;
    let cost = &ended["costDetails"];
    assert_eq!(cost["totalUsage"]["energy"], 1.0);
    assert_eq!(cost["totalCost"]["energy"]["exclTax"], 0.001);
    assert_eq!(cost["totalCost"]["typeOfCost"], "NormalCost");
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_review_transaction_end_clears_refreshed_driver_tariff() {
    let csms = v21_csms().await;
    csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo":{"status":"Accepted"},"tariff":energy("A", 1.0)}))
    });
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    set_variables(
        &csms,
        &[
            ("AuthCacheCtrlr", "Enabled", None, "true"),
            ("AuthCtrlr", "LocalPreAuthorize", None, "true"),
        ],
    )
    .await;
    install(&csms, 1, energy("default", 1.0)).await;
    let authorize = || Command2::Authorize {
        id_token: "TAG".to_owned(),
        token_type: IdTokenType::Local,
    };
    send(&station, authorize()).await;

    csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo":{"status":"Accepted"},"tariff":energy("B", 2.0)}))
    });
    let mark = csms.mark();
    send(&station, Command2::PlugIn { evse: 1 }).await;
    present(&station, "TAG", IdTokenType::Local).await;
    let started = event(&csms, mark, "eventType", "Started").await;
    assert_eq!(started["transactionInfo"]["tariffId"], "A");
    assert_eq!(
        get(&csms, 0).await["tariffAssignments"][2],
        json!({"tariffId":"B","tariffKind":"DriverTariff","idTokens":["TAG"]})
    );
    end(&csms, &station).await;
    assert_eq!(
        get(&csms, 0).await["tariffAssignments"],
        json!([{"tariffId":"default","tariffKind":"DefaultTariff","evseIds":[1]}])
    );

    csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo":{"status":"Accepted"}}))
    });
    send(&station, authorize()).await;
    let mark = csms.mark();
    send(&station, Command2::PlugIn { evse: 1 }).await;
    present(&station, "TAG", IdTokenType::Local).await;
    let started = event(&csms, mark, "eventType", "Started").await;
    assert_eq!(started["transactionInfo"]["tariffId"], "default");
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_review_offline_events_survive_downgrade_to_2_0_1() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    install(&csms, 1, energy("default", 1.0)).await;
    send(&station, Command2::Disconnect).await;
    send(&station, Command2::PlugIn { evse: 1 }).await;
    present(&station, "TAG", IdTokenType::Central).await;
    send(&station, Command2::Unplug { evse: 1 }).await;
    stop(&station, task).await;

    let state = loaded2(store(dir.path()).load2().unwrap());
    let mut expected: Vec<_> = state.queue.events.into_iter().map(|e| e.payload).collect();
    assert_eq!(expected[0]["transactionInfo"]["tariffId"], "default");
    let ended = expected.last().unwrap();
    assert_eq!(ended["eventType"], "Ended");
    assert!(ended["costDetails"].is_object());
    for payload in &mut expected {
        payload.as_object_mut().unwrap().remove("costDetails");
        let info = payload["transactionInfo"].as_object_mut().unwrap();
        info.remove("tariffId");
        info.remove("transactionLimit");
    }

    let csms = v201_csms().await;
    let (station, task) = boot(&csms, dir.path(), Protocol::V201).await;
    event(&csms, 0, "eventType", "Ended").await;
    let actual: Vec<_> = csms
        .calls_since(0)
        .into_iter()
        .filter(|e| e.action == "TransactionEvent")
        .map(|e| e.payload)
        .collect();
    assert_eq!(actual, expected);
    let schemas = SchemaRegistry::for_protocol(Protocol::V201).unwrap();
    for payload in actual {
        schemas
            .validate_request("TransactionEvent", &payload)
            .unwrap();
    }
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_review_disabled_tariffs_still_consume_central_cost_mark() {
    let csms = v21_csms().await;
    csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo":{"status":"Accepted"},"tariff":energy("invalid", -1.0)}))
    });
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    set_variables(
        &csms,
        &[
            ("AuthCacheCtrlr", "Enabled", None, "false"),
            (
                COMPONENT,
                "HandleFailedTariff",
                Some("Tariff"),
                "CentralCost",
            ),
        ],
    )
    .await;
    install(&csms, 1, energy("default", 1.0)).await;
    let result = send(
        &station,
        Command2::Authorize {
            id_token: "TAG".to_owned(),
            token_type: IdTokenType::Local,
        },
    )
    .await;
    assert!(matches!(result, Outcome2::Authorized { status, .. } if status == "Accepted"));
    csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo":{"status":"Accepted"}}))
    });

    for (enabled, tariff_id) in [("false", None), ("true", Some("default"))] {
        set_variables(&csms, &[(COMPONENT, "Enabled", Some("Tariff"), enabled)]).await;
        let mark = csms.mark();
        send(&station, Command2::PlugIn { evse: 1 }).await;
        present(&station, "TAG", IdTokenType::Local).await;
        let started = event(&csms, mark, "eventType", "Started").await;
        assert_eq!(started["transactionInfo"]["tariffId"].as_str(), tariff_id);
        end(&csms, &station).await;
    }
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_review_interval_rejects_negative_values() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    let component = json!({"name":COMPONENT});
    let interval = variable("Interval", Some("Tariff"));
    let result = call(
        &csms,
        "SetVariables",
        json!({"setVariableData":[{
            "component":component,"variable":interval,"attributeValue":"-1"
        }]}),
    )
    .await;
    assert_eq!(
        result["setVariableResult"][0]["attributeStatus"],
        "Rejected"
    );
    let result = call(
        &csms,
        "GetVariables",
        json!({"getVariableData":[{"component":component,"variable":interval}]}),
    )
    .await;
    assert_eq!(result["getVariableResult"][0]["attributeValue"], "0");
    stop(&station, task).await;
}

#[tokio::test]
async fn tariff_review_evse_above_u32_is_unknown() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().unwrap();
    let (station, task) = boot(&csms, dir.path(), Protocol::V21).await;
    enable(&csms, false).await;
    let evse = u64::from(u32::MAX) + 1;
    let rejected = json!({"status":"Rejected","statusInfo":{"reasonCode":"UnknownEVSE"}});
    assert_eq!(
        call(
            &csms,
            "SetDefaultTariff",
            json!({"evseId":evse,"tariff":energy("default",1.0)})
        )
        .await,
        rejected
    );
    assert_eq!(
        call(&csms, "GetTariffs", json!({"evseId":evse})).await,
        rejected
    );
    assert_eq!(
        call(&csms, "ClearTariffs", json!({"evseId":evse})).await,
        json!({"clearTariffsResult":[rejected]})
    );
    stop(&station, task).await;
}
