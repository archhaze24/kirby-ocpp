//! Local edits of the configuration, the local authorization list and the charging profiles.

mod station_support;

use kirby_ocpp_core::StationError;
use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::ocpp::Frame;
use kirby_ocpp_core::ocpp::messages::{ChargingProfile, IdTagInfo};
use kirby_ocpp_core::ocpp::types::AuthorizationStatus;
use kirby_ocpp_core::station::Command;
use serde_json::{Value, json};
use station_support::{Rig, tag, wait_state};

async fn booted() -> Rig {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig
}

async fn csms_payload(rig: &Rig, action: &str, request: Value) -> Value {
    match rig.csms.send_call(action, request).await.expect("a reply") {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("unexpected {}", other.to_json_string()),
    }
}

async fn refused(rig: &Rig, command: Command) -> String {
    match rig.station.send(command).await {
        Err(StationError::InvalidArgument(text)) => text,
        other => panic!("expected an InvalidArgument error, got {other:?}"),
    }
}

fn profile(id: i32, stack_level: i32) -> ChargingProfile {
    serde_json::from_value(json!({
        "chargingProfileId": id,
        "stackLevel": stack_level,
        "chargingProfilePurpose": "ChargePointMaxProfile",
        "chargingProfileKind": "Absolute",
        "chargingSchedule": {
            "chargingRateUnit": "A",
            "chargingSchedulePeriod": [{"startPeriod": 0, "limit": 16.0}, {"startPeriod": 3600, "limit": 8.0}]
        }
    }))
    .expect("valid profile")
}

#[tokio::test]
async fn config_view_lists_keys_and_a_local_change_matches_change_configuration() {
    let rig = booted().await;
    let snapshot = rig.station.snapshot();
    let item = |key: &str| snapshot.config.iter().find(|i| i.key == key).cloned();
    assert!(item("HeartbeatInterval").is_some_and(|i| !i.readonly));
    assert!(item("NumberOfConnectors").is_some_and(|i| i.readonly));

    rig.send(Command::SetConfiguration {
        key: "meterValueSampleInterval".into(),
        value: "42".into(),
    })
    .await;
    let snapshot = wait_state(&rig.station, "new value in the view", |s| {
        s.config
            .iter()
            .any(|i| i.key == "MeterValueSampleInterval" && i.value == "42")
    })
    .await;
    assert!(snapshot.config.iter().all(|i| !i.key.is_empty()));
    // The CSMS sees the same value.
    let got = csms_payload(
        &rig,
        "GetConfiguration",
        json!({"key": ["MeterValueSampleInterval"]}),
    )
    .await;
    assert_eq!(got["configurationKey"][0]["value"], "42");

    // Side effect of the key: the heartbeat interval follows HeartbeatInterval.
    rig.send(Command::SetConfiguration {
        key: "HeartbeatInterval".into(),
        value: "77".into(),
    })
    .await;
    wait_state(&rig.station, "heartbeat interval", |s| {
        s.heartbeat_interval_secs == 77
    })
    .await;

    refused(
        &rig,
        Command::SetConfiguration {
            key: "NumberOfConnectors".into(),
            value: "9".into(),
        },
    )
    .await;
    refused(
        &rig,
        Command::SetConfiguration {
            key: "HeartbeatInterval".into(),
            value: "soon".into(),
        },
    )
    .await;
    let text = refused(
        &rig,
        Command::SetConfiguration {
            key: "NoSuchKey".into(),
            value: "1".into(),
        },
    )
    .await;
    assert!(text.contains("unknown key"), "{text}");
    rig.shutdown().await;
}

#[tokio::test]
async fn local_list_edits_bump_the_version_and_validate() {
    let rig = booted().await;
    assert_eq!(rig.station.snapshot().local_list.total, 0);

    let mut info = IdTagInfo::with_status(AuthorizationStatus::Blocked);
    info.parent_id_tag = Some(tag("PARENT"));
    rig.send(Command::SetLocalListEntry {
        id_tag: tag("TAG-A"),
        info,
    })
    .await;
    rig.send(Command::SetLocalListEntry {
        id_tag: tag("TAG-B"),
        info: IdTagInfo::with_status(AuthorizationStatus::Accepted),
    })
    .await;
    let snapshot = wait_state(&rig.station, "two entries", |s| s.local_list.total == 2).await;
    assert_eq!(snapshot.local_list.version, 2);
    assert_eq!(snapshot.local_list_version, 2);
    let a = &snapshot.local_list.entries[0];
    assert_eq!(
        (a.id_tag.as_str(), a.status, a.parent_id_tag.as_deref()),
        ("TAG-A", AuthorizationStatus::Blocked, Some("PARENT"))
    );
    let version = csms_payload(&rig, "GetLocalListVersion", json!({})).await;
    assert_eq!(version["listVersion"], 2);

    // Replace TAG-A and remove TAG-B.
    rig.send(Command::SetLocalListEntry {
        id_tag: tag("TAG-A"),
        info: IdTagInfo::with_status(AuthorizationStatus::Accepted),
    })
    .await;
    rig.send(Command::RemoveLocalListEntry {
        id_tag: tag("TAG-B"),
    })
    .await;
    let snapshot = wait_state(&rig.station, "one entry", |s| {
        s.local_list.version == 4 && s.local_list.total == 1
    })
    .await;
    assert_eq!(
        snapshot.local_list.entries[0].status,
        AuthorizationStatus::Accepted
    );

    // An absent tag is an error and does not touch the version.
    let text = refused(
        &rig,
        Command::RemoveLocalListEntry {
            id_tag: tag("NOPE"),
        },
    )
    .await;
    assert!(text.contains("not in the local list"), "{text}");
    assert_eq!(rig.station.snapshot().local_list.version, 4);

    rig.send(Command::ClearLocalList).await;
    let snapshot = wait_state(&rig.station, "cleared", |s| s.local_list.total == 0).await;
    assert_eq!(snapshot.local_list.version, 0);

    // Disabled feature: refused.
    rig.send(Command::SetConfiguration {
        key: "LocalAuthListEnabled".into(),
        value: "false".into(),
    })
    .await;
    let text = refused(
        &rig,
        Command::SetLocalListEntry {
            id_tag: tag("X"),
            info: IdTagInfo::with_status(AuthorizationStatus::Accepted),
        },
    )
    .await;
    assert!(text.contains("disabled"), "{text}");
    rig.shutdown().await;
}

#[tokio::test]
async fn csms_differential_update_continues_from_a_local_edit() {
    let rig = booted().await;
    rig.send(Command::SetLocalListEntry {
        id_tag: tag("LOCAL"),
        info: IdTagInfo::with_status(AuthorizationStatus::Accepted),
    })
    .await;
    wait_state(&rig.station, "entry", |s| s.local_list.total == 1).await;
    // The CSMS-side view agrees: a differential update with the next version is accepted.
    let reply = csms_payload(
        &rig,
        "SendLocalList",
        json!({"listVersion": 2, "updateType": "Differential",
               "localAuthorizationList": [{"idTag": "OTHER", "idTagInfo": {"status": "Accepted"}}]}),
    )
    .await;
    assert_eq!(reply["status"], "Accepted");
    wait_state(&rig.station, "csms update", |s| s.local_list.total == 2).await;
    rig.shutdown().await;
}

#[tokio::test]
async fn charging_profiles_install_list_and_clear() {
    let rig = booted().await;
    rig.send(Command::InstallChargingProfile {
        connector: ConnectorId::STATION,
        profile: profile(7, 0),
    })
    .await;
    rig.send(Command::InstallChargingProfile {
        connector: ConnectorId::STATION,
        profile: profile(8, 1),
    })
    .await;
    let snapshot = wait_state(&rig.station, "two profiles", |s| {
        s.charging_profiles.len() == 2
    })
    .await;
    let p = &snapshot.charging_profiles[0];
    assert_eq!((p.id, p.stack_level, p.periods), (7, 0, 2));
    assert_eq!(p.schedule, "0s:16A 3600s:8A");
    assert!(p.json.contains("\"chargingProfileId\":7"), "{}", p.json);

    // Same rules as SetChargingProfile: a stack level above the maximum is refused.
    let text = refused(
        &rig,
        Command::InstallChargingProfile {
            connector: ConnectorId::STATION,
            profile: profile(9, 10_000),
        },
    )
    .await;
    assert!(text.contains("Rejected"), "{text}");
    // A connector that does not exist is refused too.
    refused(
        &rig,
        Command::InstallChargingProfile {
            connector: ConnectorId::new(99),
            profile: profile(10, 2),
        },
    )
    .await;
    assert_eq!(rig.station.snapshot().charging_profiles.len(), 2);

    // The CSMS sees the profiles.
    let composite = csms_payload(
        &rig,
        "GetCompositeSchedule",
        json!({"connectorId": 0, "duration": 600, "chargingRateUnit": "A"}),
    )
    .await;
    assert_eq!(composite["status"], "Accepted");

    rig.send(Command::ClearChargingProfiles { id: Some(7) })
        .await;
    let snapshot = wait_state(&rig.station, "one profile", |s| {
        s.charging_profiles.len() == 1
    })
    .await;
    assert_eq!(snapshot.charging_profiles[0].id, 8);
    let text = refused(&rig, Command::ClearChargingProfiles { id: Some(7) }).await;
    assert!(text.contains("no charging profile"), "{text}");

    rig.send(Command::ClearChargingProfiles { id: None }).await;
    wait_state(&rig.station, "none", |s| s.charging_profiles.is_empty()).await;
    // Clearing an empty store is not an error.
    rig.send(Command::ClearChargingProfiles { id: None }).await;
    rig.shutdown().await;
}
