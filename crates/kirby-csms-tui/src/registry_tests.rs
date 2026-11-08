//! The action registry against the palette and the forms: nothing is listed in one place
//! only, and every form agrees with the spec it belongs to.

use std::collections::BTreeSet;

use chrono::Utc;
use kirby_csms_core::ACTIONS_PROTOCOL;
use kirby_csms_core::registry::{self, ActionSpec, FieldType};
use kirby_ocpp_tui::text::TextInput;
use serde_json::Value;

use crate::actions::{ActionId, CATALOG, ScopedRule};
use crate::form::{Form, StopChoice, Submitted};

/// Registered actions that have no palette entry (the raw call reaches them).
const NO_PALETTE: &[&str] = &["CancelReservation", "GetLocalListVersion"];
/// Palette entries whose action takes no input and is sent without a form.
const NO_FORM: &[&str] = &["ClearCache"];

const PEM: &str = "-----BEGIN CERTIFICATE-----\n\
    MIIBszCCAVmgAwIBAgIUQ0ZWm3QmH4lQ0b1b5mCw3m0o1rYwCgYIKoZIzj0EAwIw\n\
    FjEUMBIGA1UEAwwLdGVzdCByb290IDEwHhcNMjYwMTAxMDAwMDAwWhcNMzYwMTAx\n\
    -----END CERTIFICATE-----\n";

fn specs() -> &'static [ActionSpec] {
    registry::protocol_actions(ACTIONS_PROTOCOL)
        .expect("the palette protocol has a table")
        .csms_actions
}

/// The form that the palette opens for `id`.
fn form_of(id: ActionId) -> Option<Form> {
    let now = Utc::now();
    let choice = StopChoice {
        id: 7,
        connector: 1,
        id_tag: "TAG1".to_owned(),
    };
    Some(match id {
        ActionId::RemoteStart => Form::remote_start("CP-1"),
        ActionId::RemoteStop => Form::remote_stop("CP-1", &[choice]),
        ActionId::Reset => Form::reset("CP-1"),
        ActionId::ChangeConfiguration => Form::change_configuration("CP-1"),
        ActionId::GetConfiguration => Form::get_configuration("CP-1"),
        ActionId::Trigger => Form::trigger("CP-1"),
        ActionId::Unlock => Form::unlock("CP-1", None),
        ActionId::Availability => Form::availability("CP-1"),
        ActionId::SetProfile => Form::set_profile("CP-1"),
        ActionId::DataTransfer => Form::data_transfer("CP-1"),
        ActionId::GetLog => Form::get_log("CP-1", 1),
        ActionId::InstallCertificate => Form::install_certificate("CP-1"),
        ActionId::DeleteCertificate => Form::delete_certificate("CP-1"),
        ActionId::CertificateIds => Form::get_installed_certificate_ids("CP-1"),
        ActionId::ExtendedTrigger => Form::extended_trigger("CP-1"),
        ActionId::SignedFirmware => Form::signed_update_firmware("CP-1", 1, now),
        ActionId::CertificateSigned => Form::certificate_signed("CP-1"),
        ActionId::ClearChargingProfile => Form::clear_charging_profile("CP-1"),
        ActionId::GetCompositeSchedule => Form::get_composite_schedule("CP-1"),
        ActionId::ReserveNow => Form::reserve_now("CP-1", 1, now),
        ActionId::UpdateFirmware => Form::update_firmware("CP-1", now),
        ActionId::GetDiagnostics => Form::get_diagnostics("CP-1"),
        ActionId::SendLocalList => Form::send_local_list("CP-1"),
        _ => return None,
    })
}

/// Entries that fill the required fields that have no default, by field index.
fn sample(action: &str) -> Vec<(usize, &'static str)> {
    match action {
        "RemoteStartTransaction" => vec![(0, "TAG1")],
        "ChangeConfiguration" => vec![(0, "HeartbeatInterval"), (1, "60")],
        "DataTransfer" => vec![(0, "vendor")],
        "GetLog" => vec![(2, "https://example.com/log")],
        "InstallCertificate" => vec![(1, PEM)],
        "DeleteCertificate" => vec![(1, "aa"), (2, "bb"), (3, "01")],
        "SignedUpdateFirmware" => {
            vec![(0, "https://example.com/fw"), (5, PEM), (6, "c2lnbmF0dXJl")]
        }
        "CertificateSigned" => vec![(0, PEM)],
        "ReserveNow" => vec![(2, "TAG1")],
        "UpdateFirmware" => vec![(0, "https://example.com/fw")],
        "GetDiagnostics" => vec![(0, "https://example.com/diag")],
        _ => Vec::new(),
    }
}

#[test]
fn the_palette_and_the_registry_cover_each_other() {
    let registered: BTreeSet<&str> = specs().iter().map(|spec| spec.name).collect();
    let mut shown: BTreeSet<&str> = NO_PALETTE.iter().copied().collect();
    for def in CATALOG {
        let Some(name) = def.ocpp else { continue };
        let spec = def
            .spec()
            .unwrap_or_else(|| panic!("{name} is not in the registry"));
        assert_eq!((def.label(), def.desc()), (spec.title, spec.summary));
        assert!(!spec.title.is_empty() && !spec.summary.is_empty(), "{name}");
        assert!(shown.insert(name), "{name} has two palette entries");
    }
    assert_eq!(registered, shown);
    for name in NO_PALETTE {
        assert!(registered.contains(name), "{name}");
    }
}

#[test]
fn rule_and_fault_forms_offer_the_registry_station_actions() {
    let table = registry::protocol_actions(ACTIONS_PROTOCOL).unwrap();
    for form in [Form::rule(), Form::fault()] {
        let options: Vec<&str> = form.fields[0].options.iter().map(String::as_str).collect();
        assert_eq!(options, table.station_actions);
    }
    let raw = Form::raw("CP-1", None);
    let mut names: Vec<&str> = specs().iter().map(|spec| spec.name).collect();
    names.sort_unstable();
    let options: Vec<&str> = raw.fields[0].options.iter().map(String::as_str).collect();
    assert_eq!(options, names);
}

#[test]
fn every_form_matches_its_spec() {
    let mut checked = 0;
    for def in CATALOG {
        let Some(name) = def.ocpp else { continue };
        let spec = def.spec().unwrap();
        let Some(form) = form_of(def.id) else {
            assert!(NO_FORM.contains(&name), "{name} has no form");
            assert!(spec.fields.is_empty(), "{name} takes input but has no form");
            continue;
        };
        checked += 1;
        let labels: Vec<&str> = form.fields.iter().map(|field| field.label).collect();
        let wanted: Vec<&str> = spec.fields.iter().map(|field| field.label).collect();
        assert_eq!(labels, wanted, "{name}: labels");
        for (field, want) in form.fields.iter().zip(spec.fields) {
            let at = format!("{name}.{}", want.name);
            if want.ty == FieldType::Choice && !want.choices.is_empty() {
                let options: Vec<&str> = field
                    .options
                    .iter()
                    .map(String::as_str)
                    .filter(|option| *option != "(any)")
                    .collect();
                assert_eq!(options, want.choices, "{at}: choices");
            }
            if let Some(default) = want.default {
                let shown = field.chosen().unwrap_or_else(|| field.input.text());
                assert_eq!(shown, default, "{at}: default");
            }
            if field.label.contains("(optional") {
                assert!(!want.required, "{at}: marked optional in the form");
            }
        }
    }
    assert_eq!(checked, 23);
}

/// The value at a dotted path of a JSON object.
fn at<'a>(payload: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(payload, |value, key| value.get(key))
}

#[test]
fn every_form_filled_in_submits_a_request_with_the_spec_keys() {
    for def in CATALOG {
        let Some(name) = def.ocpp else { continue };
        let Some(mut form) = form_of(def.id) else {
            continue;
        };
        for (index, text) in sample(name) {
            form.fields[index].input = TextInput::new(text);
        }
        assert!(form.validate_all(), "{name}: {:?}", form.fields);
        let Some(Submitted::Send(command)) = form.submit() else {
            panic!("{name}: the form did not submit: {:?}", form.error);
        };
        assert_eq!(command.action(), name);
        let payload = command.payload();
        for spec in def.spec().unwrap().fields.iter().filter(|f| f.required) {
            assert!(
                at(&payload, spec.name).is_some(),
                "{name}: required key {} is missing from {payload}",
                spec.name
            );
        }
    }
}

#[test]
fn rule_and_fault_forms_take_an_optional_station() {
    let mut rule = Form::rule();
    assert!(matches!(
        rule.submit(),
        Some(Submitted::Rule { station: None, .. })
    ));
    rule.fields[2].input = TextInput::new(" CP-1 ");
    assert!(matches!(
        rule.submit(),
        Some(Submitted::Rule { station: Some(id), .. }) if id == "CP-1"
    ));
    rule.fields[2].input = TextInput::new("x".repeat(129));
    assert!(!rule.validate_all());
    assert!(rule.submit().is_none());

    let mut fault = Form::fault();
    assert!(matches!(
        fault.submit(),
        Some(Submitted::Fault { station: None, .. })
    ));
    fault.fields[4].input = TextInput::new("CP-2");
    assert!(matches!(
        fault.submit(),
        Some(Submitted::Fault { station: Some(id), .. }) if id == "CP-2"
    ));
    fault.fields[4].input = TextInput::new("x".repeat(129));
    assert!(!fault.validate_all());
    assert!(fault.submit().is_none());
}

#[test]
fn the_remove_rule_form_lists_station_rules_next_to_global_ones() {
    let scoped = [ScopedRule {
        station: Some("CP-1".to_owned()),
        action: "Reset".to_owned(),
    }];
    let mut form = Form::remove_rule_scoped(&["Authorize".to_owned()], &scoped);
    let options: Vec<&str> = form.fields[0].options.iter().map(String::as_str).collect();
    assert_eq!(options, ["Authorize", "Reset@CP-1"]);
    assert_eq!(
        form.submit(),
        Some(Submitted::RemoveRule("Authorize".into()))
    );
    form.fields[0].option = 1;
    assert_eq!(
        form.submit(),
        Some(Submitted::RemoveScopedRule {
            station: Some("CP-1".to_owned()),
            action: "Reset".to_owned()
        })
    );
}
