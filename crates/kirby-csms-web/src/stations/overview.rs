//! Overview of one station: connection, boot data, connector grid and its transactions.

use leptos::prelude::*;
use serde_json::Value;

use crate::api::{StationDetail, Transaction, TransactionList, encode};
use crate::store::use_fetch;
use crate::ui::{Empty, ErrorLine, Section, delivered_wh, energy, money, power, status_class};
use crate::util::{clean, date_time};

/// One connector tile.
#[derive(Debug, Clone, PartialEq)]
struct Tile {
    label: String,
    status: String,
    detail: Option<String>,
    at: Option<String>,
    meter: Option<String>,
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// The tiles of a station: EVSEs and connectors of a 2.x station, connectors of a 1.6 station,
/// or the plain status map of the station object when the state has nothing yet.
fn tiles(detail: &StationDetail) -> Vec<Tile> {
    let state = detail.state.as_ref().unwrap_or(&Value::Null);
    let mut tiles = Vec::new();
    if let Some(evses) = state["v2"]["evses"].as_object() {
        for (evse, body) in evses {
            let meter = {
                let reading = &state["v2"]["meters"][evse];
                let energy_wh = reading["energy_wh"].as_f64().map(energy);
                let power_w = reading["power_w"].as_f64().map(power);
                match (energy_wh, power_w) {
                    (Some(e), Some(p)) => Some(format!("{e} · {p}")),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            };
            if let Some(connectors) = body["connectors"].as_object() {
                for (connector, one) in connectors {
                    tiles.push(Tile {
                        label: format!("EVSE {evse} · connector {connector}"),
                        status: one["status"].as_str().unwrap_or("Unknown").to_owned(),
                        detail: None,
                        at: one["timestamp"].as_str().map(str::to_owned),
                        meter: meter.clone(),
                    });
                }
            }
        }
    } else if let Some(connectors) = state["connectors"].as_object().filter(|c| !c.is_empty()) {
        for (connector, one) in connectors {
            let error = one["error_code"].as_str().filter(|code| *code != "NoError");
            let info = one["info"].as_str();
            tiles.push(Tile {
                label: if connector == "0" {
                    "Station (connector 0)".to_owned()
                } else {
                    format!("Connector {connector}")
                },
                status: one["status"].as_str().unwrap_or("Unknown").to_owned(),
                detail: match (error, info) {
                    (Some(error), Some(info)) => Some(format!("{error}: {info}")),
                    (Some(only), None) | (None, Some(only)) => Some(only.to_owned()),
                    (None, None) => None,
                },
                at: one["timestamp"].as_str().map(str::to_owned),
                meter: None,
            });
        }
    } else {
        for (connector, status) in &detail.station.connectors {
            tiles.push(Tile {
                label: format!("Connector {connector}"),
                status: status.clone(),
                detail: None,
                at: None,
                meter: None,
            });
        }
    }
    tiles
}

/// Boot data as label and value, in reading order; unknown values are left out.
fn boot_rows(detail: &StationDetail) -> Vec<(&'static str, String)> {
    let boot = detail
        .state
        .as_ref()
        .map_or(&Value::Null, |state| &state["boot"]);
    [
        ("Vendor", "vendor"),
        ("Model", "model"),
        ("Firmware", "firmware_version"),
        ("Charge point serial", "charge_point_serial_number"),
        ("Charge box serial", "charge_box_serial_number"),
        ("Meter type", "meter_type"),
        ("Meter serial", "meter_serial_number"),
        ("ICCID", "iccid"),
        ("IMSI", "imsi"),
        ("Registration", "registration"),
        ("Heartbeat interval", "interval"),
        ("Booted", "booted_at"),
    ]
    .into_iter()
    .filter_map(|(label, key)| {
        let value = text(&boot[key])?;
        let value = if key == "booted_at" {
            date_time(&value)
        } else if key == "interval" {
            format!("{value} s")
        } else {
            clean(&value)
        };
        Some((label, value))
    })
    .collect()
}

#[component]
pub fn OverviewTab(id: String) -> impl IntoView {
    let detail = {
        let id = id.clone();
        use_fetch::<StationDetail>(move || format!("/v1/stations/{}", encode(&id)))
    };
    let transactions = {
        let id = id.clone();
        use_fetch::<TransactionList>(move || format!("/v1/transactions?station={}", encode(&id)))
    };
    let tile_list = Memo::new(move |_| detail.data.with(|d| d.as_ref().map_or_default(tiles)));
    let boot = Memo::new(move |_| detail.data.with(|d| d.as_ref().map_or_default(boot_rows)));
    let active = Memo::new(move |_| {
        transactions.data.with(|d| {
            d.as_ref().map_or_default(|list| {
                list.transactions
                    .iter()
                    .filter(|t| t.status == "active")
                    .cloned()
                    .collect::<Vec<_>>()
            })
        })
    });
    let recent = Memo::new(move |_| {
        transactions.data.with(|d| {
            d.as_ref().map_or_default(|list| {
                let mut stopped: Vec<Transaction> = list
                    .transactions
                    .iter()
                    .filter(|t| t.status != "active")
                    .cloned()
                    .collect();
                stopped.reverse();
                stopped.truncate(5);
                stopped
            })
        })
    });
    let facts = Memo::new(move |_| {
        detail.data.with(|d| {
            d.as_ref().map_or_default(|d| {
                let heartbeat = d
                    .state
                    .as_ref()
                    .and_then(|s| s["last_heartbeat_at"].as_str())
                    .map_or_else(|| "none yet".to_owned(), date_time);
                vec![
                    (
                        "Connection",
                        if d.station.connected {
                            "connected"
                        } else {
                            "disconnected"
                        }
                        .to_owned(),
                    ),
                    (
                        "Remote address",
                        d.station
                            .remote_addr
                            .clone()
                            .unwrap_or_else(|| "-".to_owned()),
                    ),
                    ("CALLs received", d.station.received_calls.to_string()),
                    ("Last heartbeat answered", heartbeat),
                    ("Security events", d.station.security_events.to_string()),
                ]
            })
        })
    });
    view! {
        <div class="overview">
            <ErrorLine text=detail.error/>
            <div class="cards">
                <Section title="Connection">
                    <dl class="facts">
                        <For each=move || facts.get() key=|(label, _)| *label children=move |(label, value)| view! {
                            <dt>{label}</dt><dd>{value}</dd>
                        }/>
                    </dl>
                </Section>
                <Section title="Boot">
                    <Show when=move || boot.with(Vec::is_empty)>
                        <Empty>"No BootNotification received yet."</Empty>
                    </Show>
                    <dl class="facts">
                        <For each=move || boot.get() key=|(label, _)| *label children=move |(label, value)| view! {
                            <dt>{label}</dt><dd>{value}</dd>
                        }/>
                    </dl>
                </Section>
            </div>
            <Section title="Connectors">
                <Show when=move || tile_list.with(Vec::is_empty)>
                    <Empty>"No connector status reported yet."</Empty>
                </Show>
                <div class="tiles">
                    <For each=move || tile_list.get() key=|tile| tile.label.clone() children=move |tile| {
                        let label = tile.label.clone();
                        view! {
                            <div class="tile">
                                <div class="tile-label">{label.clone()}</div>
                                <div class=format!("chip chip-{}", status_class(&tile.status))>{tile.status.clone()}</div>
                                {tile.detail.clone().map(|d| view! { <div class="tile-note">{clean(&d)}</div> })}
                                {tile.meter.clone().map(|m| view! { <div class="tile-note">{m}</div> })}
                                {tile.at.clone().map(|at| view! { <div class="tile-time">{date_time(&at)}</div> })}
                            </div>
                        }
                    }/>
                </div>
            </Section>
            <Section title="Active transactions">
                <Show when=move || active.with(Vec::is_empty)>
                    <Empty>"No transaction is running."</Empty>
                </Show>
                <Show when=move || !active.with(Vec::is_empty)>
                    <TransactionTable rows=active/>
                </Show>
            </Section>
            <Show when=move || !recent.with(Vec::is_empty)>
                <Section title="Recent transactions">
                    <TransactionTable rows=recent/>
                </Section>
            </Show>
        </div>
    }
}

#[component]
fn TransactionTable(#[prop(into)] rows: Signal<Vec<Transaction>>) -> impl IntoView {
    view! {
        <div class="table-wrap">
            <table class="table">
                <thead>
                    <tr>
                        <th>"Id"</th><th>"Where"</th><th>"Token"</th><th>"Started"</th>
                        <th class="num">"Energy"</th><th class="num">"Power"</th><th class="num">"Cost"</th><th>"Status"</th>
                    </tr>
                </thead>
                <tbody>
                    <For each=move || rows.get() key=|tx| format!("{}|{}", tx.id, tx.started_at.clone().unwrap_or_default()) children=move |tx| {
                        let place = match (tx.evse, tx.connector) {
                            (Some(evse), Some(connector)) => format!("EVSE {evse} / {connector}"),
                            (None, Some(connector)) => format!("connector {connector}"),
                            (Some(evse), None) => format!("EVSE {evse}"),
                            (None, None) => "-".to_owned(),
                        };
                        view! {
                            <tr>
                                <td class="mono">{clean(&tx.id)}</td>
                                <td>{place}</td>
                                <td class="mono">{clean(tx.id_tag.as_deref().unwrap_or("-"))}</td>
                                <td>{tx.started_at.as_deref().map_or_else(|| "-".to_owned(), date_time)}</td>
                                <td class="num">{delivered_wh(&tx).map_or_else(|| "-".to_owned(), energy)}</td>
                                <td class="num">{tx.power_w.map_or_else(|| "-".to_owned(), power)}</td>
                                <td class="num">{tx.cost.as_ref().and_then(|c| c.total_incl_tax.map(|v| money(v, &c.currency))).unwrap_or_else(|| "-".to_owned())}</td>
                                <td>{match tx.stop_reason.as_deref() {
                                    Some(reason) if tx.status != "active" => format!("{} ({})", tx.status, clean(reason)),
                                    _ => tx.status.clone(),
                                }}</td>
                            </tr>
                        }
                    }/>
                </tbody>
            </table>
        </div>
    }
}
