//! Forms for ClearChargingProfile, GetCompositeSchedule, ReserveNow, UpdateFirmware,
//! GetDiagnostics and SendLocalList: typed requests, inline validation and drawing.

use chrono::{TimeZone, Utc};
use crossterm::event::KeyCode;
use kirby_ocpp_core::ocpp::messages::AuthorizationData;
use kirby_ocpp_core::ocpp::types::{
    AuthorizationStatus, ChargingProfilePurposeType, ChargingRateUnitType, UpdateType,
};
use kirby_ocpp_tui::text::TextInput;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use crate::app::{App, Effect, Mode};
use crate::command::Command;
use crate::form::{Form, Submitted};
use crate::tests::press;
use crate::ui::draw;
use crate::ui::tests::{loaded, screen};

fn set(form: &mut Form, i: usize, text: &str) {
    form.fields[i].input = TextInput::new(text);
    form.validate_all();
}

fn pick(form: &mut Form, i: usize, name: &str) {
    form.fields[i].option = form.fields[i]
        .options
        .iter()
        .position(|o| o == name)
        .unwrap();
}

fn sent(form: &mut Form) -> Command {
    match form.submit() {
        Some(Submitted::Send(c)) => c,
        other => panic!("expected a command, got {other:?} ({:?})", form.error),
    }
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap()
}

#[test]
fn clear_charging_profile_filters_are_all_optional() {
    let mut f = Form::clear_charging_profile("CP-1");
    let c = sent(&mut f);
    assert_eq!(c.action(), "ClearChargingProfile");
    assert_eq!(c.payload(), json!({}));
    set(&mut f, 0, "7");
    set(&mut f, 1, "0");
    pick(&mut f, 2, "TxDefaultProfile");
    set(&mut f, 3, "2");
    let Command::ClearChargingProfile(req) = sent(&mut f) else {
        panic!("wrong command");
    };
    assert_eq!(req.id, Some(7));
    assert_eq!(
        req.connector_id.map(kirby_ocpp_core::ids::ConnectorId::get),
        Some(0)
    );
    assert_eq!(
        req.charging_profile_purpose,
        Some(ChargingProfilePurposeType::TxDefaultProfile)
    );
    assert_eq!(req.stack_level, Some(2));
    set(&mut f, 3, "-1");
    assert!(f.submit().is_none());
    assert!(f.fields[3].error.is_some());
    assert_eq!(f.focus, 3);
}

#[test]
fn get_composite_schedule_needs_a_duration_and_takes_an_optional_unit() {
    let mut f = Form::get_composite_schedule("CP-1");
    let c = sent(&mut f);
    assert_eq!(
        c.payload(),
        json!({"connectorId": 1, "duration": 3600}),
        "defaults, no unit"
    );
    assert!(
        c.answer_is_content(&json!({})),
        "the answer opens in a popup"
    );
    pick(&mut f, 2, "W");
    set(&mut f, 1, "900");
    let c = sent(&mut f);
    assert_eq!(
        c,
        Command::GetCompositeSchedule {
            connector: 1,
            duration: 900,
            unit: Some(ChargingRateUnitType::W),
        }
    );
    assert_eq!(c.payload()["chargingRateUnit"], "W");
    set(&mut f, 1, "");
    assert!(f.submit().is_none());
    assert!(f.fields[1].error.is_some());
}

#[test]
fn reserve_now_builds_a_typed_request() {
    let mut f = Form::reserve_now("CP-1", 4, now());
    assert_eq!(f.fields[1].input.text(), "2026-05-01T11:00:00Z");
    assert_eq!(f.fields[4].input.text(), "4");
    assert!(f.submit().is_none(), "idTag is required");
    assert_eq!(f.focus, 2);
    set(&mut f, 2, &"x".repeat(21));
    assert!(f.fields[2].error.is_some());
    set(&mut f, 2, "ABC");
    set(&mut f, 3, "PARENT");
    set(&mut f, 1, "tomorrow");
    assert!(f.submit().is_none());
    assert_eq!(f.focus, 1);
    set(&mut f, 1, "2026-05-01T12:30:00Z");
    let Command::ReserveNow(req) = sent(&mut f) else {
        panic!("wrong command");
    };
    assert_eq!(req.connector_id.get(), 1);
    assert_eq!(req.id_tag.as_str(), "ABC");
    assert_eq!(
        req.parent_id_tag
            .as_ref()
            .map(kirby_ocpp_core::ids::IdTag::as_str),
        Some("PARENT")
    );
    assert_eq!(req.reservation_id.get(), 4);
    assert_eq!(
        req.expiry_date,
        Utc.with_ymd_and_hms(2026, 5, 1, 12, 30, 0).unwrap()
    );
    set(&mut f, 3, "");
    let c = sent(&mut f);
    assert!(c.payload().get("parentIdTag").is_none());
    assert_eq!(c.payload()["reservationId"], 4);
}

#[test]
fn update_firmware_and_get_diagnostics_validate_their_fields() {
    let mut f = Form::update_firmware("CP-1", now());
    assert_eq!(f.fields[1].input.text(), "2026-05-01T10:00:00Z");
    assert!(f.submit().is_none());
    assert_eq!(f.fields[0].error.as_deref(), Some("required"));
    set(&mut f, 0, "https://fw.example/1.bin");
    set(&mut f, 2, "3");
    set(&mut f, 3, "60");
    let Command::UpdateFirmware(req) = sent(&mut f) else {
        panic!("wrong command");
    };
    assert_eq!(req.location, "https://fw.example/1.bin");
    assert_eq!((req.retries, req.retry_interval), (Some(3), Some(60)));
    set(&mut f, 0, "not a uri");
    assert!(f.submit().is_none(), "the schema rejects it");
    assert!(f.error.is_some());

    let mut f = Form::get_diagnostics("CP-1");
    assert!(f.submit().is_none());
    set(&mut f, 0, "ftp://host/diag");
    set(&mut f, 3, "yesterday");
    assert!(f.submit().is_none());
    assert_eq!(f.focus, 3);
    set(&mut f, 3, "2026-01-01T00:00:00Z");
    let c = sent(&mut f);
    assert_eq!(c.action(), "GetDiagnostics");
    assert_eq!(
        c.payload(),
        json!({"location": "ftp://host/diag", "startTime": "2026-01-01T00:00:00Z"})
    );
}

#[test]
fn send_local_list_parses_entries_and_bounds_them() {
    let mut f = Form::send_local_list("CP-1");
    let Command::SendLocalList(req) = sent(&mut f) else {
        panic!("wrong command");
    };
    // The non-destructive update type is preselected.
    assert_eq!(req.update_type, UpdateType::Differential);
    assert_eq!(req.list_version, 1);
    pick(&mut f, 0, "Full");
    let Command::SendLocalList(req) = sent(&mut f) else {
        panic!("wrong command");
    };
    assert_eq!(req.update_type, UpdateType::Full);
    assert_eq!(req.local_authorization_list, None, "empty field, no list");

    pick(&mut f, 0, "Differential");
    set(&mut f, 1, "5");
    set(&mut f, 2, "AAA, BBB:blocked , CCC:-,,");
    let Command::SendLocalList(req) = sent(&mut f) else {
        panic!("wrong command");
    };
    assert_eq!(req.update_type, UpdateType::Differential);
    let list: &[AuthorizationData] = req.local_authorization_list.as_deref().unwrap();
    let view: Vec<_> = list
        .iter()
        .map(|a| (a.id_tag.as_str(), a.id_tag_info.as_ref().map(|i| i.status)))
        .collect();
    assert_eq!(
        view,
        vec![
            ("AAA", Some(AuthorizationStatus::Accepted)),
            ("BBB", Some(AuthorizationStatus::Blocked)),
            ("CCC", None),
        ]
    );
    let payload = Command::SendLocalList(req).payload();
    assert_eq!(
        payload["localAuthorizationList"][2],
        json!({"idTag": "CCC"})
    );

    set(&mut f, 2, "AAA:Nope");
    assert!(
        f.fields[2]
            .error
            .as_ref()
            .is_some_and(|e| e.contains("unknown status"))
    );
    set(&mut f, 2, &"x".repeat(21));
    assert!(f.fields[2].error.is_some());
    set(&mut f, 2, ":Accepted");
    assert!(f.fields[2].error.is_some());
    let many = (0..101)
        .map(|n| format!("T{n}"))
        .collect::<Vec<_>>()
        .join(",");
    set(&mut f, 2, &many);
    assert!(
        f.fields[2]
            .error
            .as_ref()
            .is_some_and(|e| e.contains("100"))
    );
    assert!(f.submit().is_none());
    let max = (0..100)
        .map(|n| format!("T{n}"))
        .collect::<Vec<_>>()
        .join(",");
    set(&mut f, 2, &max);
    assert!(f.submit().is_some());
}

fn form_of(app: &App) -> &Form {
    match &app.mode {
        Mode::Form(form) => form,
        other => panic!("expected a form, got {other:?}"),
    }
}

#[test]
fn hotkeys_open_the_forms_and_send_typed_commands() {
    let titles = [
        ('K', "ClearChargingProfile"),
        ('H', "GetCompositeSchedule"),
        ('V', "ReserveNow"),
        ('U', "UpdateFirmware"),
        ('O', "GetDiagnostics"),
        ('B', "SendLocalList"),
    ];
    for (key, title) in titles {
        let mut app = loaded();
        press(&mut app, KeyCode::Char(key));
        assert_eq!(form_of(&app).title, title, "key {key}");
    }
    let mut app = loaded();
    press(&mut app, KeyCode::Char('B'));
    assert_eq!(form_of(&app).kind, crate::form::FormKind::SendLocalList);
    // Skip updateType, listVersion; then type entries.
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    crate::tests::type_text(&mut app, "AAA:Invalid");
    let fx = press(&mut app, KeyCode::Enter);
    let [Effect::Send { command, .. }] = <[Effect; 1]>::try_from(fx).unwrap() else {
        panic!("expected one Send");
    };
    assert_eq!(command.action(), "SendLocalList");
    assert_eq!(
        command.payload()["localAuthorizationList"][0]["idTagInfo"]["status"],
        "Invalid"
    );
}

#[test]
fn forms_draw_their_labels() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char('B'));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| draw(f, &app)).unwrap();
    let text = screen(terminal.backend());
    assert!(
        text.contains("SendLocalList") && text.contains("listVersion"),
        "{text}"
    );
    assert!(text.contains("idTag[:status]"), "{text}");
}

#[test]
fn clear_charging_profile_without_a_filter_asks_first() {
    let mut f = Form::clear_charging_profile("CP-1");
    let all = sent(&mut f);
    assert!(all.needs_confirm());
    assert!(all.label().contains("ALL"), "{}", all.label());
    set(&mut f, 3, "2");
    let narrow = sent(&mut f);
    assert!(!narrow.needs_confirm());
    assert_eq!(narrow.label(), "ClearChargingProfile");
}
