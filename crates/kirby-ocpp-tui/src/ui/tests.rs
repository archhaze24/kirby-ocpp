use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::station::{Direction, Event, FrameRecord, LogLevel, LogRecord, Snapshot};
use kirby_ocpp_core::ws::ConnectionState;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use super::*;
use crate::app::{Msg, PendingCall, update};
use crate::help::HelpState;

fn app() -> App {
    let config = StationConfig::new("ws://csms.example/ocpp")
        .with_connectors(2)
        .with_charge_point_id("CP-1");
    let mut app = App::new(Snapshot::initial(&config), "ws://csms.example/ocpp", "Acme");
    app.snapshot.connection = ConnectionState::Connected;
    let ts = |ms: i64| Utc.timestamp_millis_opt(1_700_000_000_000 + ms).unwrap();
    let rec = |ms, dir, frame: serde_json::Value, action: &str| {
        Event::Frame(FrameRecord {
            ts: ts(ms),
            direction: dir,
            msg_id: Some("m1".into()),
            action: Some(action.into()),
            frame,
        })
    };
    app.log.push(rec(
        0,
        Direction::Out,
        json!([2, "m1", "Authorize", {"idTag": "ABC"}]),
        "Authorize",
    ));
    app.log.push(rec(
        31,
        Direction::In,
        json!([3, "m1", {"idTagInfo": {"status": "Accepted"}}]),
        "Authorize",
    ));
    app.log.push(Event::Log(LogRecord {
        ts: ts(40),
        level: LogLevel::Info,
        text: "connected".into(),
    }));
    app
}

#[test]
fn views_use_no_dark_gray_or_dim() {
    let mut a = app();
    for connection in [ConnectionState::Disconnected, ConnectionState::Connected] {
        a.snapshot.connection = connection;
        for (w, h) in [(60, 20), (110, 30), (160, 40)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, &a)).unwrap();
            for c in term.backend().buffer().content() {
                assert_ne!(c.fg, ratatui::style::Color::DarkGray, "{c:?}");
                assert_ne!(c.bg, ratatui::style::Color::DarkGray, "{c:?}");
                assert!(!c.modifier.contains(ratatui::style::Modifier::DIM), "{c:?}");
            }
        }
    }
}

fn render(app: &App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, app)).unwrap();
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(app: &mut App, c: KeyCode) {
    update(
        app,
        Msg::Key(KeyEvent::new_with_kind(
            c,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        )),
    );
}

fn keys(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
    }
}

#[test]
fn wide_layout_shows_three_columns() {
    let text = render(&app(), 150, 30);
    let row = text.lines().find(|l| l.contains("Connectors")).unwrap();
    let (c, a, l) = (
        row.find("Connectors"),
        row.find("Station"),
        row.find("Log ["),
    );
    assert!(c < a && a < l, "{row}");
    assert!(text.contains("CP-1") && text.contains("connected"));
    assert!(text.contains("Actions: Station"));
    assert!(text.contains("→ CALL Authorize idTag=ABC"));
    assert!(
        text.contains("← RES  Authorize") && text.contains("31ms"),
        "{text}"
    );
    assert!(text.contains("● connected"));
}

#[test]
fn compact_layout_stacks_the_panes() {
    let text = render(&app(), 80, 30);
    let pos = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap();
    assert!(pos("Connectors") < pos("Actions: Station") + 1);
    assert!(pos("Actions: Station") < pos("Log ["));
}

#[test]
fn hint_bar_shows_whole_hints_only() {
    let all = [
        ("Enter", "run"),
        ("↑↓", "move"),
        ("1-7", "tab"),
        ("/", "palette"),
        ("q", "quit"),
    ];
    for w in 1..80 {
        let line = hint_line(&all, w);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(width(text.trim_end()) <= w, "w={w}: {text:?}");
        let prefixes: Vec<String> = (0..=all.len())
            .map(|n| {
                all[..n]
                    .iter()
                    .map(|(k, l)| format!("{k} {l}"))
                    .collect::<Vec<_>>()
                    .join("  ")
            })
            .collect();
        assert!(prefixes.iter().any(|p| text.trim() == p), "w={w}: {text:?}");
    }
    let narrow = hint_line(&all, 20);
    let t: String = narrow.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(t.contains("Enter run") && !t.contains("quit"));
}

#[test]
fn footer_shows_disabled_reason_for_the_selected_action() {
    let mut a = app();
    a.snapshot.connection = ConnectionState::Disconnected;
    let text = render(&a, 100, 30);
    assert!(
        text.contains("BootNotification: disabled, not connected to the CSMS"),
        "{text}"
    );
}

#[test]
fn overlays_render_at_several_sizes() {
    for (w, h) in [(120, 40), (80, 24), (40, 14)] {
        let mut a = app();
        keys(&mut a, "/auth");
        let t = render(&a, w, h);
        assert!(
            t.contains("Command palette") && t.contains("Authorize"),
            "{w}x{h}\n{t}"
        );

        let mut a = app();
        keys(&mut a, "3t");
        key(&mut a, KeyCode::Enter);
        let t = render(&a, w, h);
        assert!(
            t.contains("Start transaction") && t.contains("idTag is required"),
            "{w}x{h}\n{t}"
        );

        let mut a = app();
        keys(&mut a, "3e");
        keys(&mut a, "?");
        assert!(render(&a, w, h).contains("Help"));

        let mut a = app();
        keys(&mut a, "7d");
        assert!(render(&a, w, h).contains("iagnostics"), "{w}x{h}");

        let mut a = app();
        keys(&mut a, "5c");
        assert!(render(&a, w, h).contains("[y/N]"));
    }
}

#[test]
fn log_detail_shows_pretty_json_and_the_correlated_answer() {
    let mut a = app();
    keys(&mut a, "5");
    key(&mut a, KeyCode::Up);
    key(&mut a, KeyCode::Up);
    key(&mut a, KeyCode::Enter);
    let t = render(&a, 100, 30);
    assert!(t.contains("\"idTag\": \"ABC\""), "{t}");
    assert!(
        t.contains("--- answer ---") && t.contains("\"status\": \"Accepted\""),
        "{t}"
    );
}

#[test]
fn form_shows_errors_inline_and_a_wide_char_cursor() {
    let mut a = app();
    keys(&mut a, "3a");
    update(&mut a, Msg::Paste("日本語".into()));
    let t = render(&a, 80, 24);
    assert!(t.replace(' ', "").contains("日本語"), "{t}");
    update(&mut a, Msg::Paste("x".repeat(30)));
    assert!(render(&a, 80, 24).contains("33 characters"));
}

#[test]
fn raw_form_shows_schema_error_for_the_payload() {
    let mut a = app();
    keys(&mut a, "4r");
    key(&mut a, KeyCode::Tab);
    let t = render(&a, 100, 24);
    assert!(t.contains("Raw CALL") && t.contains("Heartbeat"), "{t}");
}

#[test]
fn tiny_and_odd_sizes_never_panic_in_any_mode() {
    let setups: [&str; 9] = ["", "?", "/a", "3t", "3a", "4r", "5c", "7d", "5"];
    for setup in setups {
        let mut a = app();
        keys(&mut a, setup);
        if setup == "5" {
            key(&mut a, KeyCode::Enter);
        }
        for w in [0, 1, 2, 5, 10, 23, 24, 25, 40, 79, 109, 110, 111, 200] {
            for h in [0, 1, 3, 7, 8, 9, 12, 24, 50] {
                let _ = render(&a, w, h);
            }
        }
    }
}

#[test]
fn too_small_terminal_says_so() {
    let t = render(&app(), 20, 6);
    assert!(t.contains("need 24x8, have 20x6"), "{t}");
}

fn config_form(a: &mut App, key: &str) {
    use crate::app::Mode;
    let item = kirby_ocpp_core::station::ConfigItem {
        key: key.to_owned(),
        value: String::new(),
        readonly: false,
        hint: "text".to_owned(),
    };
    a.mode = Mode::Form(crate::forms::Form::config_value(&item, &a.snapshot));
}

#[test]
fn authorization_key_value_is_drawn_as_stars() {
    let mut a = app();
    config_form(&mut a, "AuthorizationKey");
    update(&mut a, Msg::Paste("supersecretkey12345".into()));
    let t = render(&a, 100, 30);
    assert!(!t.contains("supersecret"), "{t}");
    assert!(t.contains(&"*".repeat(19)), "{t}");
    assert!(t.contains("hidden"), "{t}");
    // Other keys stay readable.
    let mut a = app();
    config_form(&mut a, "HeartbeatInterval");
    update(&mut a, Msg::Paste("60".into()));
    assert!(render(&a, 100, 30).contains("60"));
}

#[test]
fn long_form_errors_wrap_instead_of_being_cut() {
    use crate::app::Mode;
    let mut a = app();
    let mut f = crate::forms::Form::search("");
    f.fields[0].error = Some(format!("{} TAIL-OF-THE-ERROR", "word ".repeat(18)));
    a.mode = Mode::Form(f);
    for (w, h) in [(60, 24), (80, 24), (120, 36)] {
        let t = render(&a, w, h);
        assert!(t.contains("TAIL-OF-THE-ERROR"), "{w}x{h}\n{t}");
    }
}

#[test]
fn help_opens_on_the_current_tab_and_fits_every_size() {
    use crate::app::Mode;
    let mut a = app();
    a.mode = Mode::Help(HelpState::default());
    for (w, h) in [(80, 24), (120, 36), (24, 8)] {
        let t = render(&a, w, h);
        assert!(t.contains("Help") && t.contains("This tab"), "{w}x{h}\n{t}");
        if w >= 80 {
            assert!(t.contains("connects and boots at startup"), "{w}x{h}\n{t}");
            assert!(t.contains("Everywhere"), "{w}x{h}\n{t}");
        }
        assert!(t.lines().all(|l| l.chars().count() <= usize::from(w)));
    }
}

#[test]
fn help_has_no_intro_and_two_sections() {
    let sections = help_sections(&app());
    let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["This tab", "Everywhere"]);
    for s in &sections {
        assert!(!s.lines[0].starts_with("First steps"), "{}", s.title);
    }
    assert_eq!(help_sections(&fleet_app()).len(), 2);
}

fn fleet_app() -> App {
    let mut a = app();
    a.in_fleet = true;
    a
}

fn help_text(a: &App) -> String {
    help_sections(a)
        .iter()
        .flat_map(|s| s.lines.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn help_this_tab_follows_the_open_tab() {
    let mut a = app();
    let flow = "p plug → a authorize → t start/present → m meter → e stop";
    for (tab, needle) in [
        (Tab::Station, "Send BootNotification"),
        (Tab::Connector, "Plug / unplug"),
        (Tab::Transaction, flow),
        (Tab::Maintenance, "Set diagnostics outcome"),
        (Tab::Data, "Send raw CALL"),
        (Tab::Logs, "Copy selected row"),
        (Tab::Scenarios, "Cancel scenario"),
    ] {
        a.tab = tab;
        let first = help_sections(&a).remove(0);
        let text = first.lines.join("\n");
        assert!(text.contains(needle), "{tab:?}\n{text}");
        assert_eq!(first.title, "This tab");
    }
    // The keys come from the real catalogue: every action of the tab is listed with its letter.
    a.tab = Tab::Transaction;
    let text = help_text(&a);
    for action in a.catalog.iter().filter(|x| x.tab == Tab::Transaction) {
        let line = crate::help::row(&action.key.unwrap().to_string(), &action.label);
        assert!(text.contains(&line), "{line}\n{text}");
    }
}

#[test]
fn help_everywhere_names_the_real_keys() {
    let a = app();
    let text = help_text(&a);
    for needle in [
        "1-7",
        "←/→",
        "↑/↓ j/k",
        "PgUp/PgDn",
        "Home and End",
        "[ ]",
        "Ctrl-P",
        "W ",
        "/ ",
        "z ",
        "?",
        "q ",
        "asks first",
    ] {
        assert!(text.contains(needle), "{needle}\n{text}");
    }
    assert!(!text.contains("< >"), "no fleet keys outside a fleet");
    let fleet = help_text(&fleet_app());
    assert!(fleet.contains("< >") && fleet.contains("Alt-1..9") && fleet.contains("● connected"));
}

#[test]
fn help_scrolls_and_switches_sections() {
    let mut a = app();
    keys(&mut a, "?");
    let first = render(&a, 80, 24);
    assert!(
        first.contains("This tab") && first.contains("Everywhere"),
        "{first}"
    );
    key(&mut a, KeyCode::Right);
    let second = render(&a, 80, 24);
    assert!(second.contains("Command palette"), "{second}");
    assert!(!second.contains("Send BootNotification"), "{second}");
    // At 24x8 the section does not fit: scrolling reveals the rest.
    let top = render(&a, 24, 8);
    assert!(top.contains('▼'), "{top}");
    for _ in 0..200 {
        key(&mut a, KeyCode::Down);
        let _ = render(&a, 24, 8);
    }
    let end = render(&a, 24, 8);
    assert!(end.contains('▲') && !end.contains('▼'), "{end}");
    key(&mut a, KeyCode::Esc);
    assert!(matches!(a.mode, crate::app::Mode::Normal));
}

#[test]
fn forms_use_the_whole_width_of_a_narrow_terminal() {
    let mut a = app();
    keys(&mut a, "3a");
    let t = render(&a, 40, 12);
    assert!(!t.contains("┌┌") && !t.contains("┐┐"), "{t}");
    assert!(t.contains("Authorize"), "{t}");
}

#[test]
fn long_station_id_keeps_the_connection_state_visible() {
    let mut a = app();
    a.snapshot.charge_point_id = format!("{}-tail", "L".repeat(150));
    let t = render(&a, 100, 24);
    let first = t.lines().next().unwrap_or("");
    assert!(first.contains("connected"), "{t}");
    assert!(first.contains('…'), "{t}");
}

#[test]
fn payload_text_is_displayed_inertly() {
    let mut a = app();
    a.log.push(Event::Log(LogRecord {
        ts: Utc.timestamp_millis_opt(0).unwrap(),
        level: LogLevel::Warn,
        text: "[red]x[/red] \x1b[2J{bold}".into(),
    }));
    let t = render(&a, 110, 60);
    assert!(t.contains("[red]x[/red]") && !t.contains('\x1b'));
}

#[test]
fn log_is_virtualised_and_follows_the_tail() {
    let mut a = app();
    for i in 0..500 {
        a.log.push(Event::Log(LogRecord {
            ts: Utc.timestamp_millis_opt(i).unwrap(),
            level: LogLevel::Info,
            text: format!("line-{i}"),
        }));
    }
    let t = render(&a, 130, 20);
    assert!(t.contains("line-499") && !t.contains("line-10 "));
    a.log.set_filter(crate::logview::LogFilter::Calls);
    assert!(!render(&a, 130, 20).contains("line-499"));
}

#[test]
fn connector_status_and_detail_labels_are_not_cut_or_glued() {
    for w in [110u16, 130] {
        let mut a = app();
        a.snapshot.connectors[0].status = kirby_ocpp_core::ocpp::types::ConnectorStatus::Available;
        keys(&mut a, "3");
        let t = render(&a, w, 30);
        assert!(t.contains("Available"), "{w}\n{t}");
        assert!(!t.contains("availabilityOperative"), "{t}");
    }
}

#[test]
fn active_response_rules_are_listed_in_the_station_pane() {
    let mut a = app();
    assert!(render(&a, 120, 40).contains("rules"));
    a.snapshot.response_rules = vec!["Reset=reject*3".parse().unwrap()];
    let text = render(&a, 120, 40);
    assert!(text.contains("Reset=reject*3"), "{text}");
}

#[test]
fn browser_pages_render_their_rows_and_details() {
    let mut a = app();
    update(
        &mut a,
        Msg::Snapshot(Box::new(crate::tests_support::snapshot_with_data())),
    );
    a.snapshot.connection = ConnectionState::Connected;
    keys(&mut a, "K");
    let text = render(&a, 110, 60);
    assert!(
        text.contains("1 Config keys (") && text.contains("3 Charging profiles (1)"),
        "{text}"
    );
    assert!(text.contains("HeartbeatInterval = 30"), "{text}");
    assert!(text.contains("NumberOfConnectors (ro) = 2"), "{text}");
    assert!(text.contains("accepts:"), "{text}");
    assert!(
        text.contains("Enter edit") && text.contains("Esc close"),
        "{text}"
    );

    keys(&mut a, "/numberof");
    let text = render(&a, 110, 60);
    assert!(
        text.contains("/ numberof") && !text.contains("HeartbeatInterval = 30"),
        "{text}"
    );
    key(&mut a, KeyCode::Esc);

    keys(&mut a, "2");
    let text = render(&a, 110, 60);
    assert!(text.contains("version 3, 2 entries"), "{text}");
    assert!(
        text.contains("TAG-B") && text.contains("Blocked") && text.contains("2030-01-02 03:04"),
        "{text}"
    );
    assert!(text.contains("parent PARENT"), "{text}");

    keys(&mut a, "3");
    let text = render(&a, 110, 60);
    assert!(
        text.contains("#7 conn 0 ChargePointMaxProfile Absolute stack 1  0s:16A"),
        "{text}"
    );
    assert!(text.contains("1 period(s) in A"), "{text}");
    assert!(
        text.contains("i install") && text.contains("c clear"),
        "{text}"
    );
}

#[test]
fn browser_survives_tiny_terminals_and_empty_data() {
    let mut a = app();
    for page in ["K", "L", "P"] {
        keys(&mut a, page);
        let text = render(&a, 80, 12);
        assert!(text.contains("Station data"), "{text}");
        render(&a, MIN_SIZE.0, MIN_SIZE.1);
        render(&a, 30, 9);
        key(&mut a, KeyCode::Esc);
    }
    keys(&mut a, "L");
    assert!(render(&a, 100, 20).contains("(nothing here yet)"));
}

fn with_certificates(a: &mut App) {
    use kirby_ocpp_core::ocpp::messages::{CertificateHashData, HashAlgorithm};
    use kirby_ocpp_core::station::certificates::CertificateSummary;
    a.snapshot.security_profile = 2;
    a.snapshot.certificate_request_pending = true;
    a.snapshot.pending_security_events = 3;
    a.snapshot.log_request_id = Some(41);
    a.snapshot.signed_firmware_request_id = Some(42);
    a.snapshot.certificates = vec![CertificateSummary {
        kind: "CentralSystemRootCertificate".into(),
        subject: "CN=Test Root".into(),
        serial_number: "ab12".into(),
        not_after: chrono::Utc.with_ymd_and_hms(2031, 5, 6, 0, 0, 0).unwrap(),
        hash: CertificateHashData {
            hash_algorithm: HashAlgorithm::Sha256,
            issuer_name_hash: "n".into(),
            issuer_key_hash: "k".into(),
            serial_number: "ab12".into(),
        },
    }];
}

#[test]
fn station_pane_shows_the_security_state() {
    let mut a = app();
    with_certificates(&mut a);
    let text = render(&a, 130, 40);
    assert!(text.contains("profile 2, 1 cert(s)"), "{text}");
    assert!(text.contains("3 queued, CSR pending"), "{text}");
    assert!(
        text.contains("(request 41)") && text.contains("(request 42)"),
        "{text}"
    );
}

#[test]
fn certificates_page_lists_the_store() {
    let mut a = app();
    with_certificates(&mut a);
    keys(&mut a, "Y");
    let text = render(&a, 120, 40);
    assert!(text.contains("4 Certificates (1)"), "{text}");
    assert!(text.contains("CentralSystemRootCertificate"), "{text}");
    assert!(
        text.contains("CN=Test Root") && text.contains("2031-05-06"),
        "{text}"
    );
}

#[test]
fn security_views_survive_narrow_terminals() {
    for (w, h) in [(20u16, 8u16), (40, 12), (60, 20), (80, 24)] {
        let mut a = app();
        with_certificates(&mut a);
        let _ = render(&a, w, h);
        keys(&mut a, "Y");
        let _ = render(&a, w, h);
        key(&mut a, KeyCode::Esc);
        keys(&mut a, "S");
        let _ = render(&a, w, h);
    }
}

#[test]
fn notices_with_bidi_and_line_separators_are_drawn_inert() {
    let mut a = app();
    a.notify(
        crate::app::NoticeLevel::Error,
        "bad \u{202e}evil\u{2028}end",
    );
    let t = render(&a, 110, 40);
    assert!(t.contains("evil") && !t.contains('\u{202e}') && !t.contains('\u{2028}'));
}

#[test]
fn footer_keeps_help_and_palette_hints_in_narrow_terminals() {
    let a = app();
    for w in [40_u16, 30, 24] {
        let text = render(&a, w, 30);
        let footer = text.lines().last().unwrap().to_owned();
        assert!(
            footer.contains("? help") && footer.contains(": palette"),
            "w={w}: {footer:?}"
        );
    }
}

#[test]
fn the_fleet_footer_hints_the_station_keys() {
    let mut a = app();
    let footer = |a: &App, w: u16| render(a, w, 30).lines().last().unwrap().to_owned();
    assert!(!footer(&a, 120).contains("< > station"));
    a.in_fleet = true;
    let wide = footer(&a, 120);
    assert!(
        wide.contains("< > station") && wide.contains("[ ] connector"),
        "{wide}"
    );
    // At 80 columns `[ ] connector` makes room; quit and the pinned hints stay.
    let narrow = footer(&a, 80);
    assert!(narrow.contains("< > station"), "{narrow}");
    assert!(
        narrow.contains("q quit") && narrow.contains("? help"),
        "{narrow}"
    );
}

#[test]
fn pending_command_shows_waiting_marker() {
    let mut a = app();
    a.pending.push(PendingCall {
        label: "Heartbeat".into(),
        action: Some("Heartbeat".into()),
        since: a.now,
        done: None,
    });
    let text = render(&a, 100, 30);
    assert!(text.contains("waiting... Heartbeat"), "{text}");
    a.pending.clear();
    assert!(!render(&a, 100, 30).contains("waiting..."));
}

#[test]
fn the_pending_list_shows_elapsed_time_and_the_cancel_key() {
    let mut a = app();
    a.pending.push(PendingCall {
        label: "Heartbeat".into(),
        action: Some("Heartbeat".into()),
        since: a
            .now
            .checked_sub(std::time::Duration::from_secs(7))
            .unwrap(),
        done: None,
    });
    a.pending.push(PendingCall {
        label: "start tx".into(),
        action: None,
        since: a
            .now
            .checked_sub(std::time::Duration::from_secs(65))
            .unwrap(),
        done: None,
    });
    keys(&mut a, "W");
    let text = render(&a, 100, 30);
    assert!(text.contains("Pending calls (2)"), "{text}");
    assert!(text.contains("7s  Heartbeat"), "{text}");
    assert!(text.contains("1m05s  start tx  (no cancel)"), "{text}");
    assert!(text.contains("x cancel"), "{text}");
    // The waiting line names the oldest command with its time, and points at the list.
    key(&mut a, KeyCode::Esc);
    let text = render(&a, 100, 30);
    assert!(text.contains("waiting... Heartbeat 7s (+1)"), "{text}");
    assert!(text.contains("W pending"), "{text}");
}

fn mono_buffer(app: &App) -> ratatui::buffer::Buffer {
    let mut term = Terminal::new(TestBackend::new(130, 30)).unwrap();
    term.draw(|f| draw(f, app)).unwrap();
    term.backend().buffer().clone()
}

#[test]
fn monochrome_draws_without_color_and_marks_levels_with_text() {
    use ratatui::style::{Color, Modifier};
    let mut a = app();
    a.log.push(Event::Log(LogRecord {
        ts: Utc.timestamp_millis_opt(1_700_000_000_100).unwrap(),
        level: LogLevel::Error,
        text: "it broke".into(),
    }));
    a.log.push(Event::Log(LogRecord {
        ts: Utc.timestamp_millis_opt(1_700_000_000_200).unwrap(),
        level: LogLevel::Success,
        text: "it worked".into(),
    }));
    a.mono = true;
    a.notify(NoticeLevel::Error, "failed");
    let buf = mono_buffer(&a);
    for cell in buf.content() {
        assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "{cell:?}");
    }
    let text = render(&a, 130, 30);
    // Success and failure differ by a character, not only by color.
    assert!(text.contains("x it broke"), "{text}");
    assert!(text.contains("+ it worked"), "{text}");
    assert!(text.contains("i connected"), "{text}");
    assert!(text.contains(" x failed"), "{text}");
    // Filled chips are reverse video; the header says the state in words.
    assert!(text.contains("connected"));
    assert!(
        buf.content()
            .iter()
            .any(|c| c.symbol() == "c" && c.modifier.contains(Modifier::REVERSED)),
        "the connection chip is reversed"
    );
    // Errors are bold.
    assert!(
        buf.content()
            .iter()
            .any(|c| c.symbol() == "x" && c.modifier.contains(Modifier::BOLD)),
        "an error row is bold"
    );
    // Colored mode is untouched.
    a.mono = false;
    assert!(render(&a, 130, 30).contains("● it broke"));
}

/// The keys a footer hint stands for.
fn hint_codes(hint: &str) -> Vec<KeyCode> {
    match hint {
        "Enter" => vec![KeyCode::Enter],
        "↑↓" => vec![KeyCode::Up, KeyCode::Down],
        "←→" => vec![KeyCode::Left, KeyCode::Right],
        "1-7" => ('1'..='7').map(KeyCode::Char).collect(),
        "[ ]" => vec![KeyCode::Char('['), KeyCode::Char(']')],
        other => other.chars().map(KeyCode::Char).collect(),
    }
}

/// What a key can change in the model, for "did the key do anything".
fn fingerprint(a: &App) -> String {
    format!(
        "{} {} {:?} {:?} {:?} {} {:?} {:?} {} {:?} {} {}",
        a.zoom,
        a.details.scroll,
        a.tab,
        a.focus,
        a.connector,
        a.mode.name(),
        a.action_sel,
        a.notice.as_ref().map(|n| n.text.clone()),
        a.log.follow,
        a.log.selected_index(),
        a.log.filter.label(),
        a.quit,
    )
}

#[test]
fn every_footer_hint_key_does_what_its_label_says() {
    use crate::actions::Tab;
    use crate::app::Focus;
    let fresh = |tab: Tab, focus: Focus| {
        let mut a = app();
        a.set_tab(tab);
        a.focus = focus;
        a.pending.push(PendingCall {
            label: "Heartbeat".into(),
            action: Some("Heartbeat".into()),
            since: a.now,
            done: None,
        });
        a
    };
    for tab in Tab::ALL {
        for focus in Focus::ORDER {
            for (k, label) in hints(&fresh(tab, focus)) {
                let did_something = hint_codes(k).into_iter().any(|code| {
                    let mut a = fresh(tab, focus);
                    let before = fingerprint(&a);
                    let fx = update(
                        &mut a,
                        Msg::Key(KeyEvent::new_with_kind(
                            code,
                            KeyModifiers::NONE,
                            KeyEventKind::Press,
                        )),
                    );
                    !fx.is_empty() || fingerprint(&a) != before
                });
                assert!(
                    did_something,
                    "{tab:?} / {focus:?}: the hint `{k} {label}` does nothing"
                );
            }
        }
    }
}

#[test]
fn the_log_letters_are_hinted_only_where_they_work() {
    use crate::actions::Tab;
    use crate::app::Focus;
    let mut a = app();
    a.focus = Focus::Log;
    a.tab = Tab::Station;
    let hint_keys = |a: &App| hints(a).into_iter().map(|(k, _)| k).collect::<Vec<_>>();
    assert!(!hint_keys(&a).contains(&"s"), "{:?}", hint_keys(&a));
    a.tab = Tab::Logs;
    assert!(hint_keys(&a).contains(&"s"));
}

#[test]
fn every_catalog_accelerator_of_the_logs_tab_is_in_the_help() {
    let mut a = app();
    a.tab = Tab::Logs;
    let help = help_text(&a);
    for action in a
        .catalog
        .iter()
        .filter(|x| x.tab == crate::actions::Tab::Logs)
    {
        let Some(key) = action.key else { continue };
        assert!(
            help.contains(&format!("{key} ")),
            "the help does not mention the Logs tab key {key} ({})",
            action.label
        );
    }
    assert!(help.contains("W "), "the help lists the pending key");
}

#[test]
fn time_none_drops_the_clock_of_log_rows() {
    let mut a = app();
    let row = |a: &App| {
        render(a, 130, 30)
            .lines()
            .find(|l| l.contains("→ CALL Authorize"))
            .map(str::to_owned)
            .unwrap_or_default()
    };
    assert!(row(&a).contains("22:13:20.000 → CALL"), "{}", row(&a));
    a.time = kirby_ocpp_core::timefmt::TimeMode::None;
    let without = row(&a);
    assert!(!without.contains("22:13"), "{without}");
    assert!(without.contains("→ CALL Authorize idTag=ABC"), "{without}");
}

#[test]
fn scenarios_tab_wraps_the_description_instead_of_clipping_it() {
    let mut a = app();
    keys(&mut a, "6");
    let text = render(&a, 80, 24);
    // The full description is in the details pane, split over lines.
    assert!(text.contains("Run: charge-session"), "{text}");
    assert!(
        text.contains("StartTransaction and send MeterValues"),
        "{text}"
    );
    assert!(text.contains("(connector 1)."), "{text}");
    // Also at a narrow width nothing is cut off mid-word.
    let narrow = render(&a, 50, 24);
    assert!(narrow.contains("Plug the EV in,"), "{narrow}");
}

#[test]
fn header_shows_the_protocol_chip() {
    let a = app();
    assert!(render(&a, 80, 24).contains("OCPP 1.6"));
}

#[test]
fn detail_popup_is_as_tall_as_its_content() {
    use crate::app::Detail;
    let mut a = app();
    a.mode = Mode::Detail(Detail {
        title: "Heartbeat".to_owned(),
        lines: vec!["{}".to_owned(), String::new()],
        scroll: 0,
        copy: String::new(),
    });
    let text = render(&a, 80, 24);
    let top = text.lines().position(|l| l.contains("Heartbeat")).unwrap();
    let bottom = text
        .lines()
        .skip(top)
        .position(|l| l.contains('└'))
        .map(|n| n + top)
        .unwrap();
    // Two content rows plus the two borders.
    assert_eq!(bottom - top + 1, 4, "{text}");
}

#[test]
fn unfiltered_palette_orders_by_task_and_shows_keys() {
    // Opened on the Scenarios tab, which owns none of the actions compared here.
    let mut a = app();
    keys(&mut a, "6:");
    let text = render(&a, 100, 60);
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}\n{text}"))
    };
    assert!(at("Station: Connect") < at("Connector: Plug / unplug"));
    assert!(at("Connector: Plug / unplug") < at("Transaction: Authorize token..."));
    assert!(at("Transaction: Authorize token...") < at("Connector: Inject or clear fault"));
    assert!(at("Connector: Inject or clear fault") < at("Station: Edit config keys..."));
    assert!(at("Scenarios: Run: charge-session") < at("Station: Connect"));
    // A letter is shown only where it works: on the current tab.
    assert!(text.contains("o  Scenarios: Run: charge-session"), "{text}");
    assert!(text.contains("   Station: Connect"), "{text}");
    assert!(!text.contains("c  Station: Connect"), "{text}");
    assert!(text.contains("type to filter"), "{text}");
}

#[test]
fn the_palette_lists_the_current_tabs_actions_first_with_their_letters() {
    let mut a = app();
    keys(&mut a, "3:");
    let text = render(&a, 100, 60);
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}\n{text}"))
    };
    assert!(
        text.contains("a  Transaction: Authorize token..."),
        "{text}"
    );
    assert!(at("Transaction: Send custom meter values") < at("Station: Connect"));
    assert!(
        !text.contains("b  Station: Send BootNotification"),
        "{text}"
    );
}

#[test]
fn zoom_gives_the_focused_pane_the_whole_body_and_escape_restores_it() {
    let mut a = app();
    assert_eq!(a.focus, crate::app::Focus::Actions);
    keys(&mut a, "z");
    let zoomed = render(&a, 80, 24);
    assert!(zoomed.contains("Actions: Station"), "{zoomed}");
    assert!(
        !zoomed.contains("Connectors") && !zoomed.contains("Log"),
        "{zoomed}"
    );
    assert!(zoomed.contains("z/Esc back"), "{zoomed}");
    // 15 actions fit into the zoomed pane: no cut-off marker.
    assert!(!zoomed.contains("more"), "{zoomed}");
    key(&mut a, KeyCode::Esc);
    assert!(!a.zoom);
    let normal = render(&a, 80, 24);
    assert!(
        normal.contains("Connectors") && normal.contains("Log"),
        "{normal}"
    );
    // The footer hint needs room that 80 columns do not have; the help names `z`.
    assert!(render(&a, 120, 24).contains(" z zoom"));
    // The zoom follows the focus: another pane is zoomed once the focus moves.
    keys(&mut a, "z");
    key(&mut a, KeyCode::Right);
    let log = render(&a, 80, 24);
    assert!(log.contains("Log") && !log.contains("Actions"), "{log}");
    keys(&mut a, "z");
    assert!(!a.zoom);
}

#[test]
fn cut_panes_say_how_much_is_hidden_and_the_title_counts_the_rows() {
    let a = app();
    let small = render(&a, 80, 24);
    let actions = small
        .lines()
        .find(|l| l.contains("Actions: Station"))
        .unwrap_or_else(|| panic!("{small}"));
    assert!(actions.contains(" 1/"), "{actions}");
    assert!(small.contains("▼ ") && small.contains(" more"), "{small}");
    // A wide terminal shows every action: no counter, no marker in the pane.
    let wide = render(&a, 200, 50);
    let actions = wide
        .lines()
        .find(|l| l.contains("Actions: Station"))
        .unwrap_or_else(|| panic!("{wide}"));
    assert!(
        !actions.contains("1/") && !actions.contains('▼'),
        "{actions}"
    );
    // The log pane marks what is above when the newest row is followed.
    let mut busy = app();
    for i in 0..40 {
        busy.log.push(Event::Log(LogRecord {
            ts: Utc.timestamp_millis_opt(1_700_000_001_000 + i).unwrap(),
            level: LogLevel::Info,
            text: format!("row {i}"),
        }));
    }
    let text = render(&busy, 80, 24);
    assert!(text.contains("▲ ") && text.contains("more"), "{text}");
}

#[test]
fn the_detail_popup_stops_scrolling_at_its_last_page() {
    let mut a = app();
    a.mode = crate::app::Mode::Detail(crate::app::Detail {
        title: "Row".to_owned(),
        lines: (0..40).map(|i| format!("line {i}")).collect(),
        scroll: 0,
        copy: String::new(),
    });
    let _ = render(&a, 80, 24);
    for _ in 0..100 {
        key(&mut a, KeyCode::Down);
        let _ = render(&a, 80, 24);
    }
    let crate::app::Mode::Detail(d) = &a.mode else {
        panic!("the popup closed");
    };
    let at_end = d.scroll;
    assert!(
        at_end < 40 - 1,
        "scroll ran to the last line, not the last page: {at_end}"
    );
    key(&mut a, KeyCode::Up);
    let crate::app::Mode::Detail(d) = &a.mode else {
        panic!("the popup closed");
    };
    assert_eq!(d.scroll, at_end - 1, "one Up moves at once");
}

#[test]
fn a_long_prefill_opens_at_its_start() {
    let path = format!("/very/long/directory/{}/log.jsonl", "nested/".repeat(20));
    let mut a = app();
    a.mode = crate::app::Mode::Form(crate::forms::Form::export(&path));
    for (w, h) in [(80, 24), (200, 50)] {
        let text = render(&a, w, h);
        assert!(text.contains("/very/long/directory/"), "{w}x{h}\n{text}");
    }
    // Editing starts from the end, where the value is cut.
    key(&mut a, KeyCode::End);
    assert!(!render(&a, 80, 24).contains("/very/long/directory/"));
}

#[test]
fn the_start_and_authorize_forms_open_with_the_last_id_tag() {
    let mut a = app();
    let mut fx = Vec::new();
    a.run_action(crate::actions::ActionId::StartTx, &mut fx);
    let crate::app::Mode::Form(f) = &a.mode else {
        panic!("no form");
    };
    assert_eq!(f.fields[1].input.text(), "", "nothing used yet");
    a.mode = crate::app::Mode::Normal;
    a.apply_form(
        crate::forms::Submitted::Authorize {
            id_tag: kirby_ocpp_core::ids::IdTag::new("TAG9").unwrap(),
        },
        &mut fx,
    );
    for id in [
        crate::actions::ActionId::StartTx,
        crate::actions::ActionId::Authorize,
    ] {
        a.run_action(id, &mut fx);
        let crate::app::Mode::Form(f) = &a.mode else {
            panic!("no form");
        };
        assert!(f.fields.iter().any(|x| x.input.text() == "TAG9"), "{id:?}");
        // Typing replaces the remembered tag.
        let focused = &f.fields[f.focus];
        assert!(focused.fresh && focused.input.text() == "TAG9");
        a.mode = crate::app::Mode::Normal;
    }
}
