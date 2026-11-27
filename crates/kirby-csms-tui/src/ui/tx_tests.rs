//! What the transactions table, the summary line and the transaction popup show for energy,
//! power, cost and the OCPP 2.1 settings, at 80x24 and wide.

use crossterm::event::KeyCode;
use kirby_csms_core::{TransactionStatus, Tx2Record, TxCost, TxLimit};
use kirby_ocpp_core::ocpp::v2::types::StopReason;

use super::tests::{app_v2_on, render, tx2};
use crate::app::{App, Focus, Mode};
use crate::tests::{at, press};
use crate::txview;

fn cost(currency: &str, excl: f64, incl: f64) -> TxCost {
    TxCost {
        currency: currency.to_owned(),
        total_excl_tax: Some(excl),
        total_incl_tax: Some(incl),
    }
}

/// A running 2.1 transaction with every new field set, and an ended one with a stale power
/// reading. Newest first, the running one is the first row.
fn running_and_ended() -> (Tx2Record, Tx2Record) {
    let mut running = tx2("run-1", Some(1), "TOK-1");
    running.meter_start_wh = Some(100);
    running.energy_wh = Some(1500.0);
    running.power_w = Some(7200.0);
    running.tariff_id = Some("T-1".to_owned());
    running.transaction_limit = Some(TxLimit {
        max_cost: Some(5.0),
        max_energy: Some(10_000.0),
        max_time: Some(3600),
        max_soc: Some(80),
    });
    running.cost = Some(cost("EUR", 2.67, 3.2));
    running.priority_charging = true;
    let mut ended = tx2("end-1", Some(1), "TOK-2");
    ended.status = TransactionStatus::Stopped;
    ended.stopped_at = Some(at(60));
    ended.stopped_reason = Some(StopReason::Local);
    ended.meter_start_wh = Some(100);
    ended.meter_stop_wh = Some(1900);
    ended.energy_wh = Some(1850.0);
    ended.power_w = Some(5555.0);
    ended.cost = Some(cost("EUR", 4.0, 4.5));
    (running, ended)
}

fn app_with(running: Tx2Record, ended: Tx2Record, w: u16, h: u16) -> App {
    let mut app = app_v2_on("ocpp2.1");
    app.state
        .as_mut()
        .and_then(|s| s.v2.as_mut())
        .expect("a 2.x station")
        .transactions = vec![ended, running];
    app.size = (w, h);
    app
}

fn popup(app: &mut App, down: usize) -> Vec<String> {
    press(app, KeyCode::Right);
    assert_eq!(app.focus, Focus::Details);
    for _ in 0..down {
        press(app, KeyCode::Down);
    }
    press(app, KeyCode::Enter);
    let Mode::Detail(d) = &app.mode else {
        panic!("no popup: {:?}", app.mode);
    };
    d.lines.clone()
}

fn has(lines: &[String], text: &str) -> bool {
    lines.iter().any(|l| l == text)
}

#[test]
fn the_popup_of_a_running_2_1_transaction_shows_tariff_limit_cost_and_priority() {
    let (running, ended) = running_and_ended();
    let mut app = app_with(running, ended, 80, 24);
    let lines = popup(&mut app, 0);
    for want in [
        "Tariff      T-1",
        "Limit       max energy 10000 Wh, max time 3600 s, max cost 5.00 EUR, max SoC 80 %",
        "Cost        3.20 EUR incl. tax, 2.67 EUR excl. tax (running)",
        "Priority    charging",
        "Delivered   1400 Wh (meter 1500 Wh, start 100 Wh)",
        "Power       7200 W",
    ] {
        assert!(has(&lines, want), "missing {want:?}: {lines:#?}");
    }
    // The summary comes first; the record follows.
    let record = lines.iter().position(|l| l == "--- record ---").unwrap();
    assert!(
        record > 5 && lines[record + 1].starts_with('{'),
        "{lines:#?}"
    );
    // The popup shows its first rows at 80x24.
    let screen = render(&app, 80, 24);
    assert!(screen.contains("Delivered"), "{screen}");
    assert!(screen.contains("Cost        3.20 EUR"), "{screen}");
}

#[test]
fn the_popup_of_an_ended_transaction_says_final_and_has_no_power() {
    let (running, ended) = running_and_ended();
    let mut app = app_with(running, ended, 80, 24);
    let lines = popup(&mut app, 1);
    assert!(
        has(
            &lines,
            "Cost        4.50 EUR incl. tax, 4.00 EUR excl. tax (final)"
        ),
        "{lines:#?}"
    );
    // The stop reading wins over the last sample.
    assert!(
        has(&lines, "Delivered   1800 Wh (meter 1900 Wh, start 100 Wh)"),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("Power")), "{lines:#?}");
    for absent in ["Tariff", "Limit", "Priority"] {
        assert!(
            !lines.iter().any(|l| l.starts_with(absent)),
            "{absent}: {lines:#?}"
        );
    }
}

#[test]
fn the_table_shows_delivered_energy_and_no_power_for_an_ended_transaction() {
    for (w, h) in [(80, 24), (160, 40)] {
        let (running, ended) = running_and_ended();
        let app = app_with(running, ended, w, h);
        let screen = render(&app, w, h);
        let running_row = screen.lines().find(|l| l.contains("TOK-1")).unwrap();
        let ended_row = screen.lines().find(|l| l.contains("TOK-2")).unwrap();
        assert!(running_row.contains("1400"), "{w}x{h}:\n{screen}");
        assert!(running_row.contains("7200"), "{w}x{h}:\n{screen}");
        assert!(ended_row.contains("1800"), "{w}x{h}:\n{screen}");
        assert!(!screen.contains("5555"), "{w}x{h}:\n{screen}");
        assert!(!screen.contains("1500"), "{w}x{h}:\n{screen}");
        // The summary line carries the running cost.
        assert!(screen.contains("cost 3.20 EUR"), "{w}x{h}:\n{screen}");
    }
}

#[test]
fn a_wide_table_shows_the_cost_next_to_the_state() {
    let (running, ended) = running_and_ended();
    let app = app_with(running, ended, 160, 40);
    let screen = render(&app, 160, 40);
    let ended_row = screen.lines().find(|l| l.contains("TOK-2")).unwrap();
    assert!(
        ended_row.contains("ended Local \u{b7} 4.50 EUR"),
        "{screen}"
    );
    let running_row = screen.lines().find(|l| l.contains("TOK-1")).unwrap();
    assert!(running_row.contains("active \u{b7} 3.20 EUR"), "{screen}");
}

#[test]
fn an_unknown_start_shows_the_register_marked_and_says_so_in_the_popup() {
    let mut running = tx2("run-1", Some(1), "TOK-1");
    running.energy_wh = Some(1500.0);
    let (_, ended) = running_and_ended();
    let mut app = app_with(running, ended, 80, 24);
    let screen = render(&app, 80, 24);
    let row = screen.lines().find(|l| l.contains("TOK-1")).unwrap();
    assert!(row.contains("1500*"), "{screen}");
    let lines = popup(&mut app, 0);
    assert!(
        has(
            &lines,
            "Delivered   unknown (start reading missing); meter 1500 Wh"
        ),
        "{lines:#?}"
    );
}

#[test]
fn a_transaction_without_new_data_shows_nothing_new() {
    let (running, ended) = running_and_ended();
    let mut plain = tx2("run-2", Some(1), "TOK-3");
    plain.meter_start_wh = Some(0);
    plain.energy_wh = Some(10.0);
    let mut app = app_with(plain, ended, 80, 24);
    let _ = running;
    let screen = render(&app, 80, 24);
    assert!(!screen.contains("cost"), "{screen}");
    let lines = popup(&mut app, 0);
    for absent in ["Tariff", "Limit", "Cost", "Priority"] {
        assert!(
            !lines.iter().any(|l| l.starts_with(absent)),
            "{absent}: {lines:#?}"
        );
    }
}

#[test]
fn a_cost_without_a_currency_is_the_number_alone() {
    let bare = cost("", 1.0, 1.234);
    assert_eq!(txview::cost_short(&bare).as_deref(), Some("1.23"));
    let eur = cost("EUR", 1.0, 1.234);
    assert_eq!(txview::cost_short(&eur).as_deref(), Some("1.23 EUR"));
    let hostile = cost("E\u{1b}UR", 1.0, 2.0);
    assert_eq!(
        txview::cost_short(&hostile).as_deref(),
        Some("2.00 E\u{b7}UR")
    );
    // Excl. tax only.
    let net = TxCost {
        currency: "USD".to_owned(),
        total_excl_tax: Some(7.0),
        total_incl_tax: None,
    };
    assert_eq!(txview::cost_short(&net).as_deref(), Some("7.00 USD"));
    assert_eq!(
        txview::cost_short(&TxCost {
            currency: "USD".into(),
            total_excl_tax: None,
            total_incl_tax: None
        }),
        None
    );
}

#[test]
fn the_running_cost_needs_one_currency() {
    let a = cost("EUR", 1.0, 1.5);
    let b = cost("EUR", 2.0, 2.5);
    let c = cost("USD", 2.0, 2.5);
    assert_eq!(
        txview::running_cost([&a, &b].into_iter()).as_deref(),
        Some("4.00 EUR")
    );
    assert_eq!(txview::running_cost([&a, &c].into_iter()), None);
    assert_eq!(txview::running_cost(std::iter::empty()), None);
}

#[test]
fn energy_and_power_cells_follow_the_data() {
    let delivered = txview::energy(Some(100), None, Some(1500.0), false);
    assert_eq!(txview::wh_cell(&delivered), "1400");
    let unknown = txview::energy(None, None, Some(1500.0), false);
    assert_eq!(txview::wh_cell(&unknown), "1500*");
    // A register below the start (a meter reset) is not shown as delivered energy.
    let reset = txview::energy(Some(5000), None, Some(100.0), false);
    assert_eq!(txview::wh_cell(&reset), "100*");
    assert_eq!(
        txview::wh_cell(&txview::energy(Some(1), None, None, false)),
        "-"
    );
    assert_eq!(txview::w_cell(Some(7200.0), false), "7200");
    assert_eq!(txview::w_cell(Some(7200.0), true), "-");
    assert_eq!(txview::w_cell(None, false), "-");
}

#[test]
fn a_1_6_transaction_shows_delivered_energy_and_a_summary_in_its_popup() {
    let mut app = crate::ui::tests::loaded();
    app.size = (80, 24);
    let screen = render(&app, 80, 24);
    // Meter start 100, register 1500: 1400 delivered.
    assert!(screen.contains("1400"), "{screen}");
    let lines = popup(&mut app, 0);
    assert!(
        has(&lines, "Delivered   1400 Wh (meter 1500 Wh, start 100 Wh)"),
        "{lines:#?}"
    );
    for absent in ["Tariff", "Limit", "Cost", "Priority"] {
        assert!(
            !lines.iter().any(|l| l.starts_with(absent)),
            "{absent}: {lines:#?}"
        );
    }
}
