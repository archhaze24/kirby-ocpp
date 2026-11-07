//! Security Whitepaper forms, details and notices: model tests, drawing tests and the executor
//! against a real engine and a real station.

#![allow(
    clippy::assert_is_empty,
    reason = "test fixtures read better field by field"
)]

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::KeyCode;
use kirby_csms_core::{
    CsmsConfig, CsmsEvent, CsmsHandle, CsmsServer, SecurityEventRecord, SecurityProfile,
    StationState,
};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::messages::{CertificateUse, ExtendedMessageTrigger};
use kirby_ocpp_core::station::Command as StationCommand;
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use kirby_ocpp_tui::app::NoticeLevel;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::app::{App, Effect, Mode, Msg, update};
use crate::command::Command;
use crate::tests::{app_with, at, press, type_text, wait_booted};
use crate::ui::draw;
use crate::ui::tests::{loaded, screen, state};
use crate::{Exec, execute};

const STEP: Duration = Duration::from_secs(10);

const PEM_BODY: &str = "MIIBszCCAVmgAwIBAgIUQ0ZWm3QmH4lQ0b1b5mCw3m0o1rYwCgYIKoZIzj0EAwIw\n\
    FjEUMBIGA1UEAwwLdGVzdCByb290IDEwHhcNMjYwMTAxMDAwMDAwWhcNMzYwMTAx\n\
    MDAwMDAwWjAWMRQwEgYDVQQDDAt0ZXN0IHJvb3QgMTBZMBMGByqGSM49AgEGCCqG";

fn pem() -> String {
    format!("-----BEGIN CERTIFICATE-----\n{PEM_BODY}\n-----END CERTIFICATE-----\n")
}

fn notice(app: &App) -> String {
    app.notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

fn only_send(fx: Vec<Effect>) -> Command {
    match <[Effect; 1]>::try_from(fx) {
        Ok([Effect::Send { command, .. }]) => command,
        other => panic!("expected one Send, got {other:?}"),
    }
}

fn form_of(app: &App) -> &crate::form::Form {
    match &app.mode {
        Mode::Form(form) => form,
        other => panic!("expected a form, got {other:?}"),
    }
}

fn tabs(app: &mut App, n: usize) {
    for _ in 0..n {
        press(app, KeyCode::Tab);
    }
}

// ---- forms ---------------------------------------------------------------------------------

#[test]
fn get_log_builds_a_typed_request_and_validates_inline() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(form_of(&app).title, "GetLog");
    // The location is required: the form stays open and focuses it.
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(form_of(&app).focus, 2);
    assert_eq!(form_of(&app).fields[2].error.as_deref(), Some("required"));
    tabs(&mut app, 0);
    type_text(&mut app, "sftp://u@host/logs");
    // A bad optional timestamp is an inline error too.
    tabs(&mut app, 3);
    type_text(&mut app, "yesterday");
    assert!(form_of(&app).fields[5].error.is_some());
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(form_of(&app).focus, 5);
    press(&mut app, KeyCode::Char('u'));
    // Ctrl-U clears; here a plain 'u' was typed, so clear by backspacing.
    for _ in 0..16 {
        press(&mut app, KeyCode::Backspace);
    }
    type_text(&mut app, "2026-01-01T00:00:00Z");
    // Pick SecurityLog on the first field.
    for _ in 0..5 {
        press(&mut app, KeyCode::BackTab);
    }
    press(&mut app, KeyCode::Right);
    let command = only_send(press(&mut app, KeyCode::Enter));
    let Command::GetLog(req) = &command else {
        panic!("{command:?}");
    };
    assert_eq!(req.request_id, 1);
    assert_eq!(req.log.remote_location, "sftp://u@host/logs");
    assert!(req.log.oldest_timestamp.is_some() && req.log.latest_timestamp.is_none());
    assert_eq!(command.payload()["logType"], "SecurityLog");
    // The next form suggests the next request id.
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(form_of(&app).fields[1].input.text(), "2");
}

#[test]
fn certificate_forms_rebuild_pasted_pem_and_check_it() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char('I'));
    assert_eq!(form_of(&app).title, "InstallCertificate");
    tabs(&mut app, 1);
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(form_of(&app).fields[1].error.as_deref(), Some("required"));
    // Garbage and a missing footer are inline errors.
    update(&mut app, Msg::Paste("hello".into()));
    assert!(form_of(&app).fields[1].error.is_some());
    for _ in 0..5 {
        press(&mut app, KeyCode::Backspace);
    }
    // A multi-line paste (line breaks become spaces) is rebuilt into a valid PEM.
    update(&mut app, Msg::Paste(pem()));
    assert_eq!(form_of(&app).fields[1].error, None);
    press(&mut app, KeyCode::BackTab);
    press(&mut app, KeyCode::Right);
    let command = only_send(press(&mut app, KeyCode::Enter));
    let Command::InstallCertificate {
        certificate_type,
        certificate,
    } = &command
    else {
        panic!("{command:?}");
    };
    assert_eq!(
        *certificate_type,
        CertificateUse::ManufacturerRootCertificate
    );
    assert!(certificate.starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(certificate.ends_with("-----END CERTIFICATE-----\n"));
    assert!(
        certificate
            .lines()
            .all(|l| l.len() <= 64 || l.starts_with("-----"))
    );
    assert_eq!(certificate.lines().count(), 5);

    // Too long: over 5500 characters.
    press(&mut app, KeyCode::Char('I'));
    tabs(&mut app, 1);
    let long = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----",
        "A".repeat(6000)
    );
    update(&mut app, Msg::Paste(long));
    assert!(
        form_of(&app).fields[1]
            .error
            .as_ref()
            .is_some_and(|e| e.contains("5500"))
    );
    press(&mut app, KeyCode::Esc);

    // A chain of two blocks keeps both.
    press(&mut app, KeyCode::Char('X'));
    update(&mut app, Msg::Paste(format!("{}{}", pem(), pem())));
    let command = only_send(press(&mut app, KeyCode::Enter));
    let Command::CertificateSigned { chain } = &command else {
        panic!("{command:?}");
    };
    assert_eq!(chain.matches("BEGIN CERTIFICATE").count(), 2);
    assert_eq!(command.action(), "CertificateSigned");
}

#[test]
fn delete_ids_trigger_and_firmware_forms() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char('d'));
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    // The failed submit focused the first empty hash.
    assert_eq!(form_of(&app).focus, 1);
    type_text(&mut app, "ab12");
    tabs(&mut app, 1);
    type_text(&mut app, "cd34");
    tabs(&mut app, 1);
    type_text(&mut app, "01ff");
    let command = only_send(press(&mut app, KeyCode::Enter));
    assert_eq!(
        command.payload()["certificateHashData"]["serialNumber"],
        "01ff"
    );
    // The schema limits the hash length: a form-level error, nothing sent.
    press(&mut app, KeyCode::Char('d'));
    tabs(&mut app, 1);
    type_text(&mut app, &"a".repeat(130));
    tabs(&mut app, 1);
    type_text(&mut app, "cd34");
    tabs(&mut app, 1);
    type_text(&mut app, "01ff");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(form_of(&app).error.is_some());
    press(&mut app, KeyCode::Esc);

    press(&mut app, KeyCode::Char('L'));
    let command = only_send(press(&mut app, KeyCode::Enter));
    assert_eq!(
        command,
        Command::GetInstalledCertificateIds(CertificateUse::CentralSystemRootCertificate)
    );

    press(&mut app, KeyCode::Char('E'));
    let command = only_send(press(&mut app, KeyCode::Enter));
    assert_eq!(
        command,
        Command::ExtendedTriggerMessage {
            message: ExtendedMessageTrigger::SignChargePointCertificate,
            connector: None
        }
    );

    press(&mut app, KeyCode::Char('S'));
    assert_eq!(
        form_of(&app).fields[1].input.text(),
        app.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    type_text(&mut app, "https://host/fw.bin");
    tabs(&mut app, 5);
    update(&mut app, Msg::Paste(pem()));
    tabs(&mut app, 1);
    type_text(&mut app, "c2ln");
    let command = only_send(press(&mut app, KeyCode::Enter));
    let payload = command.payload();
    assert_eq!(payload["firmware"]["location"], "https://host/fw.bin");
    assert_eq!(payload["firmware"]["signature"], "c2ln");
    assert_eq!(payload["requestId"], 1);
}

#[test]
fn security_events_open_in_a_scrollable_popup() {
    let mut app = loaded();
    press(&mut app, KeyCode::Char('e'));
    assert!(
        notice(&app).contains("no security events"),
        "{}",
        notice(&app)
    );
    let mut st = state();
    for n in 0..5 {
        st.security_events.push_back(SecurityEventRecord {
            event_type: format!("Event{n}"),
            timestamp: at(n),
            tech_info: (n == 4).then(|| "lid\u{1b}[31m opened".to_owned()),
            received_at: at(n),
        });
    }
    st.security_events_total = 5;
    update(
        &mut app,
        Msg::Refreshed {
            states: vec![("CP-1".into(), Some(Box::new(st)))],
            info: None,
        },
    );
    press(&mut app, KeyCode::Char('e'));
    let Mode::Detail(detail) = &app.mode else {
        panic!("detail");
    };
    assert!(detail.title.starts_with("Security events of CP-1"));
    assert!(detail.lines[1].contains("Event4") && detail.lines[1].contains("lid"));
    assert!(!detail.lines.iter().any(|l| l.contains('\u{1b}')));
    assert_eq!(detail.lines.len(), 6);
}

#[test]
fn auth_failures_are_counted_and_noticed_once_per_second() {
    let (mut app, _) = app_with(&["CP-1"]);
    let refused = |station: &str, reason: &str| {
        Msg::Event(CsmsEvent::AuthFailed {
            station: Some(station.to_owned()),
            remote_addr: "10.0.0.9:5000".parse().unwrap(),
            reason: reason.to_owned(),
        })
    };
    update(&mut app, refused("CP-\u{1b}[2J-X", "invalid credentials"));
    let first = notice(&app);
    assert!(
        first.contains("invalid credentials") && !first.contains('\u{1b}'),
        "{first}"
    );
    assert_eq!(
        app.notice.as_ref().map(|n| n.level),
        Some(NoticeLevel::Error)
    );
    app.notice = None;
    update(&mut app, refused("CP-2", "invalid credentials"));
    assert_eq!(app.auth_failures, 2);
    assert!(app.notice.is_none(), "throttled");
    update(&mut app, Msg::Tick(at(2)));
    update(
        &mut app,
        Msg::Event(CsmsEvent::AuthFailed {
            station: None,
            remote_addr: "10.0.0.9:5001".parse().unwrap(),
            reason: "TLS handshake failed".to_owned(),
        }),
    );
    assert!(notice(&app).contains("unknown station"));
    assert_eq!(app.auth_failures, 3);
}

#[test]
fn refused_without_a_station_does_not_repeat_the_word_refused() {
    let (mut app, _) = app_with(&["CP-1"]);
    update(
        &mut app,
        Msg::Event(CsmsEvent::AuthFailed {
            station: None,
            remote_addr: "10.0.0.9:5001".parse().unwrap(),
            reason: "refused: station id longer than 128 bytes".to_owned(),
        }),
    );
    let text = notice(&app);
    assert_eq!(
        text,
        "refused unknown station from 10.0.0.9:5001: station id longer than 128 bytes"
    );
}

// ---- drawing ---------------------------------------------------------------------------------

fn render(app: &App, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    screen(terminal.backend())
}

#[test]
fn details_and_header_show_security_information() {
    let mut app = loaded();
    app.security_profile = 2;
    app.auth_failures = 3;
    let mut st = state();
    for n in 0..5 {
        st.security_events.push_back(SecurityEventRecord {
            event_type: format!("Event{n}"),
            timestamp: at(n),
            tech_info: Some("tech".to_owned()),
            received_at: at(n),
        });
    }
    st.security_events_total = 5;
    st.log_status = Some(kirby_csms_core::LogReport {
        status: kirby_ocpp_core::ocpp::messages::UploadLogStatus::Uploaded,
        request_id: Some(7),
        at: at(0),
    });
    st.certificate_request = Some(kirby_csms_core::CertificateRequest {
        csr: "x".to_owned(),
        received_at: at(0),
    });
    st.installed_certificates.insert(
        "CentralSystemRootCertificate".to_owned(),
        kirby_csms_core::InstalledCertificates {
            status: kirby_ocpp_core::ocpp::messages::GetInstalledCertificateStatus::Accepted,
            certificates: (0..2)
                .map(|n| kirby_ocpp_core::ocpp::messages::CertificateHashData {
                    hash_algorithm: kirby_ocpp_core::ocpp::messages::HashAlgorithm::Sha256,
                    issuer_name_hash: format!("{n}"),
                    issuer_key_hash: "ab".to_owned(),
                    serial_number: "01".to_owned(),
                })
                .collect(),
            total: 2,
            at: at(0),
        },
    );
    update(
        &mut app,
        Msg::Refreshed {
            states: vec![("CP-1".into(), Some(Box::new(st)))],
            info: None,
        },
    );
    let text = render(&app, 200, 40);
    assert!(
        text.contains("installed CentralSystemRootCertificate: 2"),
        "{text}"
    );
    assert!(text.contains("3 auth failures"), "{text}");
    assert!(text.contains("security profile 2"), "{text}");
    assert!(text.contains("security events 5"), "{text}");
    assert!(text.contains("log Uploaded #7"), "{text}");
    assert!(text.contains("CSR received"), "{text}");
    assert!(
        text.contains("Event4 tech") && text.contains("Event2 tech"),
        "{text}"
    );
    assert!(!text.contains("Event1 tech"), "only the last three: {text}");
}

#[test]
fn pem_forms_draw_in_small_terminals() {
    let mut app = loaded();
    for key in ['I', 'S', 'X', 'l', 'd'] {
        press(&mut app, KeyCode::Char(key));
        tabs(&mut app, 1);
        update(&mut app, Msg::Paste(pem()));
        for (w, h) in [(24, 8), (30, 10), (60, 20), (120, 40)] {
            let text = render(&app, w, h);
            assert!(!text.is_empty());
        }
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }
    let text = render(&app, 120, 40);
    assert!(text.contains("CP-1"));
}

// ---- real engine and station --------------------------------------------------------------------

async fn start_engine(tweak: impl FnOnce(&mut CsmsConfig)) -> CsmsHandle {
    let mut config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
    tweak(&mut config);
    CsmsServer::start(config).await.expect("engine starts")
}

fn station_config(csms: &CsmsHandle, id: &str, dir: &std::path::Path) -> StationConfig {
    let mut config = StationConfig::new(format!("{}/ocpp", csms.url()))
        .with_charge_point_id(id)
        .with_call_timeout_ms(2_000)
        .with_reconnect(ReconnectConfig {
            enabled: false,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .with_state_dir(dir);
    config.ws_ping_interval_secs = 0;
    config
}

async fn wait_state(
    csms: &CsmsHandle,
    events: &mut tokio::sync::broadcast::Receiver<CsmsEvent>,
    id: &str,
    check: impl Fn(&StationState) -> bool,
) {
    timeout(STEP, async {
        loop {
            if csms.state(id).is_some_and(|s| check(&s)) {
                return;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("state reached in time");
}

async fn run(
    app: &mut App,
    csms: &CsmsHandle,
    exec: &mut Exec,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
    command: Command,
) {
    app.pending = 1;
    let effect = Effect::Send {
        station: "CP-S".into(),
        command,
    };
    assert!(execute(effect, csms, exec).await.is_empty());
    let msg = timeout(STEP, rx.recv())
        .await
        .expect("the answer arrives")
        .expect("channel open");
    assert!(matches!(msg, Msg::CommandDone { .. }));
    update(app, msg);
}

#[tokio::test]
async fn security_commands_run_against_a_real_station() {
    let csms = start_engine(|_| {}).await;
    let mut events = csms.subscribe();
    let dir = tempfile::tempdir().unwrap();
    let (station, join) = Station::spawn(
        station_config(&csms, "CP-S", dir.path()),
        Arc::new(SystemClock),
    );
    station.connect().await.expect("connect");
    wait_state(&csms, &mut events, "CP-S", |s| s.boot.is_some()).await;
    wait_booted(&station).await;

    let (mut app, _) = app_with(&["CP-S"]);
    let (mut exec, mut rx) = Exec::new();

    // ExtendedTriggerMessage(SignChargePointCertificate) makes the station send a CSR.
    run(
        &mut app,
        &csms,
        &mut exec,
        &mut rx,
        Command::ExtendedTriggerMessage {
            message: ExtendedMessageTrigger::SignChargePointCertificate,
            connector: None,
        },
    )
    .await;
    assert_eq!(notice(&app), "ExtendedTriggerMessage on CP-S: Accepted");
    wait_state(&csms, &mut events, "CP-S", |s| {
        s.certificate_request.is_some()
    })
    .await;

    // InstallCertificate, then the list of installed ones opens in a popup.
    let root = rcgen::generate_simple_self_signed(vec!["root".to_owned()]).unwrap();
    let kind = CertificateUse::CentralSystemRootCertificate;
    run(
        &mut app,
        &csms,
        &mut exec,
        &mut rx,
        Command::InstallCertificate {
            certificate_type: kind,
            certificate: root.cert.pem(),
        },
    )
    .await;
    assert_eq!(notice(&app), "InstallCertificate on CP-S: Accepted");
    run(
        &mut app,
        &csms,
        &mut exec,
        &mut rx,
        Command::GetInstalledCertificateIds(kind),
    )
    .await;
    let Mode::Detail(detail) = &app.mode else {
        panic!("expected the popup, got {:?}", app.mode);
    };
    assert!(
        detail.lines.iter().any(|l| l == "1 certificates"),
        "{:?}",
        detail.lines
    );
    assert!(detail.lines.iter().any(|l| l.contains("SHA256")));

    station.shutdown().await.expect("station shutdown");
    join.await.expect("actor ends");
    csms.shutdown().await;
}

#[tokio::test]
async fn a_refused_station_shows_up_as_an_auth_failure() {
    let csms = start_engine(|c| {
        c.security.profile = SecurityProfile::BasicAuth;
        c.security
            .keys
            .insert("CP-S".to_owned(), "0123456789abcdef".to_owned());
    })
    .await;
    let mut events = csms.subscribe();
    let dir = tempfile::tempdir().unwrap();
    let (station, join) = Station::spawn(
        station_config(&csms, "CP-S", dir.path()),
        Arc::new(SystemClock),
    );
    for (key, value) in [
        ("AuthorizationKey", "ffffffffffffffff"),
        ("SecurityProfile", "1"),
    ] {
        station
            .send(StationCommand::SetConfiguration {
                key: key.to_owned(),
                value: value.to_owned(),
            })
            .await
            .expect("configured");
    }
    station.connect().await.ok();
    let event = timeout(STEP, async {
        loop {
            if let Ok(event @ CsmsEvent::AuthFailed { .. }) = events.recv().await {
                return event;
            }
        }
    })
    .await
    .expect("the refusal is reported");

    let mut app = App::new(csms.url(), at(0));
    app.security_profile = csms.security_profile().number();
    update(&mut app, Msg::Event(event));
    assert_eq!(app.security_profile, 1);
    assert_eq!(app.auth_failures, 1);
    assert!(
        notice(&app).contains("invalid credentials"),
        "{}",
        notice(&app)
    );
    assert!(render(&app, 120, 30).contains("1 auth failure"));

    station.shutdown().await.ok();
    join.await.ok();
    csms.shutdown().await;
}
