//! The power model: charging profiles limit the simulated power and the energy follows it.

mod station_support;

use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::ocpp::types::Measurand;
use kirby_ocpp_core::station::Command;
use serde_json::{Value, json};
use station_support::{Rig, STEP, incrementing};

const C1: ConnectorId = ConnectorId::new(1);

/// A station whose nominal power is high enough for energy to show up within milliseconds.
async fn fast_rig(nominal_power_w: u32) -> Rig {
    let mut rig = Rig::with(incrementing(), |mut config| {
        config.nominal_power_w = nominal_power_w;
        config
    })
    .await;
    rig.connect_booted().await;
    rig
}

fn profile_w(id: i32, limit: f64) -> Value {
    json!({
        "connectorId": 1,
        "csChargingProfiles": {
            "chargingProfileId": id, "stackLevel": 0,
            "chargingProfilePurpose": "TxDefaultProfile", "chargingProfileKind": "Absolute",
            "chargingSchedule": {"chargingRateUnit": "W",
                "chargingSchedulePeriod": [{"startPeriod": 0, "limit": limit}]}
        }
    })
}

fn profile_a(id: i32, limit: f64) -> Value {
    json!({
        "connectorId": 1,
        "csChargingProfiles": {
            "chargingProfileId": id, "stackLevel": 0,
            "chargingProfilePurpose": "TxDefaultProfile", "chargingProfileKind": "Absolute",
            "chargingSchedule": {"chargingRateUnit": "A",
                "chargingSchedulePeriod": [{"startPeriod": 0, "limit": limit}]}
        }
    })
}

/// One `MeterValues` command with `measurands`; returns `value` of each, by measurand name.
async fn sample(rig: &Rig, measurands: &[Measurand]) -> Vec<String> {
    let mark = rig.csms.mark();
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta: 0,
        measurands: Some(measurands.to_vec()),
    })
    .await;
    let sent = rig
        .csms
        .wait_for_call_after(mark, "MeterValues", |_| true, STEP)
        .await
        .expect("MeterValues");
    sent["meterValue"][0]["sampledValue"]
        .as_array()
        .expect("sampled values")
        .iter()
        .map(|v| v["value"].as_str().expect("string value").to_owned())
        .collect()
}

fn register(rig: &Rig) -> u64 {
    rig.station.snapshot().connector(C1).unwrap().meter_wh
}

/// Samples until the register passed `above` (the integration needs real time to pass).
async fn register_above(rig: &Rig, above: u64) -> u64 {
    tokio::time::timeout(STEP * 5, async {
        loop {
            sample(rig, &[Measurand::EnergyActiveImportRegister]).await;
            let wh = register(rig);
            if wh > above {
                return wh;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("energy grows while charging")
}

#[tokio::test]
async fn readings_follow_the_charging_limit() {
    let rig = fast_rig(7_200).await;
    rig.begin(1, "TAG1").await;
    let all = [
        Measurand::PowerActiveImport,
        Measurand::CurrentImport,
        Measurand::Voltage,
        Measurand::PowerOffered,
        Measurand::CurrentOffered,
    ];
    // No profile: the nominal power, 7200 W at 230 V on one phase.
    assert_eq!(
        sample(&rig, &all).await,
        ["7200", "31.3", "230", "7200", "31.3"]
    );

    // 3000 W: power, current and the offer all follow.
    rig.csms
        .expect_status("SetChargingProfile", profile_w(1, 3000.0), "Accepted")
        .await
        .unwrap();
    assert_eq!(
        sample(&rig, &all).await,
        ["3000", "13", "230", "3000", "13"]
    );

    // Amperes are converted with the configured voltage: 16 A x 230 V = 3680 W.
    rig.csms
        .expect_status("SetChargingProfile", profile_a(2, 16.0), "Accepted")
        .await
        .unwrap();
    assert_eq!(
        sample(&rig, &all).await,
        ["3680", "16", "230", "3680", "16"]
    );

    // Clearing the profile returns to the nominal power.
    rig.csms
        .expect_status("ClearChargingProfile", json!({"id": 2}), "Accepted")
        .await
        .unwrap();
    assert_eq!(
        sample(&rig, &all).await,
        ["7200", "31.3", "230", "7200", "31.3"]
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn energy_stops_growing_under_a_zero_limit_and_resumes_after_clearing() {
    // 3.6 GW nominal: 1 000 000 Wh per second, so milliseconds are enough.
    let rig = fast_rig(3_600_000_000).await;
    rig.begin(1, "TAG1").await;
    let grown = register_above(&rig, 0).await;

    rig.csms
        .expect_status("SetChargingProfile", profile_w(1, 0.0), "Accepted")
        .await
        .unwrap();
    // The first sample after the change may still hold energy from before it; after that
    // the register is frozen because the limit is zero.
    sample(&rig, &[Measurand::PowerActiveImport]).await;
    let frozen = register(&rig);
    assert!(frozen >= grown);
    for _ in 0..3 {
        let power = sample(&rig, &[Measurand::PowerActiveImport]).await;
        assert_eq!(power, ["0"]);
        assert_eq!(register(&rig), frozen);
    }

    rig.csms
        .expect_status("ClearChargingProfile", json!({"id": 1}), "Accepted")
        .await
        .unwrap();
    let resumed = register_above(&rig, frozen).await;
    assert!(resumed > frozen);
    rig.shutdown().await;
}

#[tokio::test]
async fn nothing_is_integrated_while_not_charging() {
    let rig = fast_rig(3_600_000_000).await;
    // Plugged in, no transaction: the connector is Preparing/Available, not Charging.
    rig.send(Command::PlugIn { connector: C1 }).await;
    for _ in 0..3 {
        let power = sample(&rig, &[Measurand::PowerActiveImport]).await;
        assert_eq!(power, ["0"]);
    }
    assert_eq!(register(&rig), 0);
    rig.shutdown().await;
}

#[tokio::test]
async fn interval_measurand_is_the_energy_since_the_previous_sample() {
    let rig = fast_rig(3_600_000_000).await;
    rig.begin(1, "TAG1").await;
    let first = register_above(&rig, 0).await;
    let values = sample(
        &rig,
        &[
            Measurand::EnergyActiveImportRegister,
            Measurand::EnergyActiveImportInterval,
        ],
    )
    .await;
    let register_now: u64 = values[0].parse().unwrap();
    let interval: u64 = values[1].parse().unwrap();
    // The interval covers only what happened since the previous sample.
    assert!(register_now > first, "{values:?}");
    assert_eq!(interval, register_now - first, "{values:?}");
    rig.shutdown().await;
}
