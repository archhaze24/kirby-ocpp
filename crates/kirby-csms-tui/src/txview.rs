//! How the CSMS UI words what a transaction carries: delivered energy, power, the OCPP 2.1
//! tariff, limit, cost and priority. The transactions table, the summary line and the
//! transaction popup share these functions, so they always agree.

use chrono::{DateTime, Utc};
use kirby_csms_core::{TransactionRecord, TransactionStatus, Tx2Record, TxCost, TxLimit};
use kirby_ocpp_core::ocpp::v2::types::WireEnum;
use kirby_ocpp_tui::text::{sanitize, truncate};

/// The energy of a transaction. `delivered` is the register minus the start reading; it is
/// `None` when either is unknown (or the register went backwards), and then `register` is
/// the only number there is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Energy {
    pub delivered: Option<f64>,
    /// The latest meter register, Wh. An ended transaction uses its stop reading.
    pub register: Option<f64>,
    pub start: Option<i64>,
}

#[allow(
    clippy::cast_precision_loss,
    reason = "meter readings are far below 2^53 Wh"
)]
pub fn energy(start: Option<i64>, stop: Option<i64>, last: Option<f64>, ended: bool) -> Energy {
    let register = if ended {
        stop.map(|v| v as f64).or(last)
    } else {
        last
    };
    let delivered = start
        .zip(register)
        .map(|(s, r)| r - s as f64)
        .filter(|d| *d >= 0.0);
    Energy {
        delivered,
        register,
        start,
    }
}

pub fn energy_of_1x(tx: &TransactionRecord) -> Energy {
    energy(
        Some(tx.meter_start),
        tx.meter_stop,
        tx.energy_wh,
        tx.status == TransactionStatus::Stopped,
    )
}

pub fn energy_of_2x(tx: &Tx2Record) -> Energy {
    energy(
        tx.meter_start_wh,
        tx.meter_stop_wh,
        tx.energy_wh,
        tx.status == TransactionStatus::Stopped,
    )
}

/// The `Wh` cell: the delivered energy, or the meter register followed by `*` when the start
/// reading is unknown, or `-`.
pub fn wh_cell(e: &Energy) -> String {
    match (e.delivered, e.register) {
        (Some(d), _) => format!("{d:.0}"),
        (None, Some(r)) => format!("{r:.0}*"),
        (None, None) => "-".to_owned(),
    }
}

/// The `W` cell: the last power reading of a running transaction; an ended one has none.
pub fn w_cell(power: Option<f64>, ended: bool) -> String {
    match power {
        Some(p) if !ended => format!("{p:.0}"),
        _ => "-".to_owned(),
    }
}

/// The total of a cost: with tax when the station sent it, else without.
pub fn total(cost: &TxCost) -> Option<f64> {
    cost.total_incl_tax.or(cost.total_excl_tax)
}

fn currency(cost: &TxCost) -> String {
    truncate(&sanitize(cost.currency.trim()), 8)
}

/// `3.20 EUR`; the number alone when the currency is empty.
pub fn money(value: f64, currency: &str) -> String {
    if currency.is_empty() {
        format!("{value:.2}")
    } else {
        format!("{value:.2} {currency}")
    }
}

/// The compact cost of a transaction: `3.20 EUR`.
pub fn cost_short(cost: &TxCost) -> Option<String> {
    total(cost).map(|t| money(t, &currency(cost)))
}

/// The running cost of all active transactions together, when each of them reported a cost in
/// one and the same currency.
pub fn running_cost<'a>(costs: impl Iterator<Item = &'a TxCost>) -> Option<String> {
    let mut sum = 0.0;
    let mut unit: Option<String> = None;
    for cost in costs {
        let value = total(cost)?;
        let cur = currency(cost);
        if unit.as_ref().is_some_and(|u| *u != cur) {
            return None;
        }
        unit = Some(cur);
        sum += value;
    }
    unit.map(|u| money(sum, &u))
}

/// `3.20 EUR incl. tax, 2.67 EUR excl. tax`, with `running` or `final` after it.
fn cost_long(cost: &TxCost, ended: bool) -> Option<String> {
    let cur = currency(cost);
    let mut parts = Vec::new();
    if let Some(v) = cost.total_incl_tax {
        parts.push(format!("{} incl. tax", money(v, &cur)));
    }
    if let Some(v) = cost.total_excl_tax {
        parts.push(format!("{} excl. tax", money(v, &cur)));
    }
    (!parts.is_empty()).then(|| {
        format!(
            "{} ({})",
            parts.join(", "),
            if ended { "final" } else { "running" }
        )
    })
}

/// The limits the station reported, as `max energy 10000 Wh, max time 3600 s`.
fn limit_text(limit: &TxLimit, currency: &str) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(v) = limit.max_energy {
        parts.push(format!("max energy {v:.0} Wh"));
    }
    if let Some(v) = limit.max_time {
        parts.push(format!("max time {v} s"));
    }
    if let Some(v) = limit.max_cost {
        parts.push(format!("max cost {}", money(v, currency)));
    }
    if let Some(v) = limit.max_soc {
        parts.push(format!("max SoC {v} %"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn time(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

fn row(out: &mut Vec<String>, label: &str, value: impl AsRef<str>) {
    out.push(format!("{label:<11} {}", sanitize(value.as_ref())));
}

fn energy_rows(out: &mut Vec<String>, e: &Energy) {
    let wh = |v: f64| format!("{v:.0} Wh");
    match (e.delivered, e.register) {
        (Some(d), Some(r)) => row(
            out,
            "Delivered",
            format!(
                "{} (meter {}, start {} Wh)",
                wh(d),
                wh(r),
                e.start.unwrap_or(0)
            ),
        ),
        (None, Some(r)) => row(
            out,
            "Delivered",
            format!("unknown (start reading missing); meter {}", wh(r)),
        ),
        _ => {}
    }
}

/// The readable summary of an OCPP 1.6 transaction, for the top of its popup.
pub fn summary_1x(tx: &TransactionRecord) -> Vec<String> {
    let ended = tx.status == TransactionStatus::Stopped;
    let mut out = Vec::new();
    row(&mut out, "Transaction", tx.id.to_string());
    row(&mut out, "State", if ended { "ended" } else { "active" });
    row(&mut out, "Connector", tx.connector.to_string());
    row(&mut out, "idTag", &tx.id_tag);
    row(&mut out, "Started", time(tx.started_at));
    if let Some(at) = tx.stopped_at {
        row(&mut out, "Stopped", time(at));
    }
    energy_rows(&mut out, &energy_of_1x(tx));
    if let Some(p) = tx.power_w.filter(|_| !ended) {
        row(&mut out, "Power", format!("{p:.0} W"));
    }
    out
}

/// The readable summary of an OCPP 2.x transaction, for the top of its popup. The tariff,
/// limit, cost and priority lines appear only when the station reported them.
pub fn summary_2x(tx: &Tx2Record) -> Vec<String> {
    let ended = tx.status == TransactionStatus::Stopped;
    let mut out = Vec::new();
    row(&mut out, "Transaction", &tx.id);
    row(
        &mut out,
        "State",
        if ended {
            format!("ended {}", tx.stopped_reason.map_or("-", |r| r.as_str()))
        } else {
            "active".to_owned()
        },
    );
    row(
        &mut out,
        "EVSE/conn",
        format!(
            "{}/{}",
            tx.evse.map_or_else(|| "-".to_owned(), |v| v.to_string()),
            tx.connector
                .map_or_else(|| "-".to_owned(), |v| v.to_string())
        ),
    );
    if let Some(token) = &tx.id_token {
        row(&mut out, "idToken", token);
    }
    if let Some(state) = tx.charging_state {
        row(&mut out, "Charging", state.as_str());
    }
    row(&mut out, "Started", time(tx.started_at));
    if let Some(at) = tx.stopped_at {
        row(&mut out, "Stopped", time(at));
    }
    energy_rows(&mut out, &energy_of_2x(tx));
    if let Some(p) = tx.power_w.filter(|_| !ended) {
        row(&mut out, "Power", format!("{p:.0} W"));
    }
    if let Some(id) = &tx.tariff_id {
        row(&mut out, "Tariff", id);
    }
    if let Some(text) = tx
        .transaction_limit
        .as_ref()
        .and_then(|limit| limit_text(limit, &tx.cost.as_ref().map(currency).unwrap_or_default()))
    {
        row(&mut out, "Limit", text);
    }
    if let Some(text) = tx.cost.as_ref().and_then(|c| cost_long(c, ended)) {
        row(&mut out, "Cost", text);
    }
    if tx.priority_charging {
        row(&mut out, "Priority", "charging");
    }
    out
}

/// The `State` cell of a 2.x transaction: the state, with the cost after it when it fits.
pub fn state_cell_2x(tx: &Tx2Record, width: usize) -> String {
    let state = if tx.status == TransactionStatus::Active {
        "active".to_owned()
    } else {
        format!("ended {}", tx.stopped_reason.map_or("-", |r| r.as_str()))
    };
    let state = sanitize(&state);
    let full = tx
        .cost
        .as_ref()
        .and_then(cost_short)
        .map(|c| format!("{state} \u{b7} {c}"));
    match full {
        Some(text) if text.chars().count() <= width => text,
        _ => truncate(&state, width),
    }
}
