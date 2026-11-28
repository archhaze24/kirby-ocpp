//! The Transactions view: the transactions of every station, with filters and a CSV download
//! built in the browser.

use leptos::prelude::*;

use crate::api::{Transaction, TransactionList};
use crate::store::{View, use_fetch, use_store};
use crate::ui::{Empty, ErrorLine, delivered_wh, energy, money, power};
use crate::util::{clean, date_time, download, now_rfc3339};

/// Columns of the CSV file: the ones of `GET /v1/transactions?format=csv`, then the energy
/// delivered and the cost.
const CSV_HEADER: [&str; 18] = [
    "station",
    "id",
    "connector",
    "id_tag",
    "id_tag_status",
    "meter_start",
    "started_at",
    "status",
    "meter_stop",
    "stopped_at",
    "stop_reason",
    "energy_wh",
    "power_w",
    "last_meter_at",
    "evse",
    "delivered_wh",
    "cost_currency",
    "cost_incl_tax",
];

/// A text cell: quoted when needed, and a leading `=`, `+`, `-` or `@` gets a `'` so a
/// spreadsheet does not read it as a formula (as the API's CSV does).
fn csv_text(cell: &str) -> String {
    let cleaned: String = cell
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect();
    let cell = if cleaned.starts_with(['=', '+', '-', '@']) {
        format!("'{cleaned}")
    } else {
        cleaned
    };
    if cell.contains([',', '"']) {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell
    }
}

fn csv_number<T: ToString>(value: Option<T>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

fn csv_row(tx: &Transaction) -> Vec<String> {
    let text = |value: &Option<String>| value.as_deref().map(csv_text).unwrap_or_default();
    vec![
        csv_text(&tx.station),
        csv_text(&tx.id),
        csv_number(tx.connector),
        text(&tx.id_tag),
        text(&tx.id_tag_status),
        csv_number(tx.meter_start),
        text(&tx.started_at),
        csv_text(&tx.status),
        csv_number(tx.meter_stop),
        text(&tx.stopped_at),
        text(&tx.stop_reason),
        csv_number(tx.energy_wh),
        csv_number(tx.power_w),
        text(&tx.last_meter_at),
        csv_number(tx.evse),
        csv_number(delivered_wh(tx)),
        tx.cost
            .as_ref()
            .map(|c| csv_text(&c.currency))
            .unwrap_or_default(),
        csv_number(tx.cost.as_ref().and_then(|c| c.total_incl_tax)),
    ]
}

/// The CSV document of `rows`: a header row and CRLF line ends (RFC 4180).
fn csv_document(rows: &[Transaction]) -> String {
    let mut out = CSV_HEADER.join(",");
    out.push_str("\r\n");
    for tx in rows {
        out.push_str(&csv_row(tx).join(","));
        out.push_str("\r\n");
    }
    out
}

#[component]
pub fn TransactionsView() -> impl IntoView {
    let store = use_store();
    let state = RwSignal::new(String::new());
    let station_filter = RwSignal::new(String::new());
    let fetched = use_fetch::<TransactionList>(move || {
        let state = state.get();
        if state.is_empty() {
            "/v1/transactions".to_owned()
        } else {
            format!("/v1/transactions?state={state}")
        }
    });
    let rows = Memo::new(move |_| {
        let needle = station_filter.get().trim().to_lowercase();
        fetched.data.with(|d| {
            d.as_ref()
                .map(|list| {
                    let mut rows: Vec<Transaction> = list
                        .transactions
                        .iter()
                        .filter(|t| needle.is_empty() || t.station.to_lowercase().contains(&needle))
                        .cloned()
                        .collect();
                    // Newest first.
                    rows.sort_by(|a, b| b.started_at.cmp(&a.started_at));
                    rows
                })
                .unwrap_or_default()
        })
    });
    let truncated = move || {
        fetched
            .data
            .with(|d| d.as_ref().is_some_and(|l| l.truncated))
    };
    let save = move |_| {
        let document = rows.with(|rows| csv_document(rows));
        let stamp = now_rfc3339().replace(':', "-");
        download(&format!("transactions-{stamp}.csv"), "text/csv", &document);
    };
    view! {
        <div class="transactions">
            <header class="page-head">
                <h2 class="page-title">"Transactions"</h2>
                <span class="muted">{move || format!("{} shown", rows.with(Vec::len))}</span>
                <span class="spacer"></span>
                <button class="button" on:click=move |_| fetched.reload()>"Refresh"</button>
                <button class="button primary" on:click=save disabled=move || rows.with(Vec::is_empty)>"Download CSV"</button>
            </header>
            <div class="toolbar">
                <label class="field-inline">
                    <span>"State"</span>
                    <select class="input" prop:value=move || state.get()
                        on:change=move |e| state.set(event_target_value(&e))>
                        <option value="" selected=move || state.get().is_empty()>"all"</option>
                        <option value="active" selected=move || state.get() == "active">"active"</option>
                        <option value="stopped" selected=move || state.get() == "stopped">"stopped"</option>
                    </select>
                </label>
                <label class="field-inline grow">
                    <span>"Station"</span>
                    <input class="input" type="search" placeholder="Filter by station id" autocomplete="off"
                        prop:value=move || station_filter.get()
                        on:input=move |e| station_filter.set(event_target_value(&e))/>
                </label>
            </div>
            <ErrorLine text=fetched.error/>
            <Show when=truncated>
                <p class="note">"The server returned only the newest transactions. Narrow the filters to see the rest."</p>
            </Show>
            <Show when=move || rows.with(Vec::is_empty)>
                <Empty>"No transactions."</Empty>
            </Show>
            <div class="table-wrap">
                <table class="table">
                    <thead>
                        <tr>
                            <th>"Station"</th><th>"Id"</th><th>"Where"</th><th>"Token"</th><th>"Started"</th><th>"Stopped"</th>
                            <th class="num">"Energy"</th><th class="num">"Power"</th><th class="num">"Cost"</th><th>"Status"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For each=move || rows.get() key=|tx| format!("{}|{}|{}", tx.station, tx.id, tx.started_at.clone().unwrap_or_default()) children=move |tx| {
                            let station = tx.station.clone();
                            let place = match (tx.evse, tx.connector) {
                                (Some(evse), Some(connector)) => format!("EVSE {evse} / {connector}"),
                                (None, Some(connector)) => format!("connector {connector}"),
                                (Some(evse), None) => format!("EVSE {evse}"),
                                (None, None) => "-".to_owned(),
                            };
                            view! {
                                <tr>
                                    <td>
                                        <button class="link" on:click=move |_| {
                                            store.selected.set(Some(station.clone()));
                                            store.view.set(View::Stations);
                                        }>{clean(&tx.station)}</button>
                                    </td>
                                    <td class="mono">{clean(&tx.id)}</td>
                                    <td>{place}</td>
                                    <td class="mono">{clean(tx.id_tag.as_deref().unwrap_or("-"))}</td>
                                    <td>{tx.started_at.as_deref().map_or_else(|| "-".to_owned(), date_time)}</td>
                                    <td>{tx.stopped_at.as_deref().map_or_else(|| "-".to_owned(), date_time)}</td>
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
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_text_quotes_and_defuses_formulas() {
        assert_eq!(csv_text("CP-1"), "CP-1");
        assert_eq!(csv_text("=CP1"), "'=CP1");
        assert_eq!(csv_text("a,b"), "\"a,b\"");
        assert_eq!(csv_text("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_text("a\nb"), "a\u{fffd}b");
    }

    #[test]
    fn csv_has_the_header_and_crlf() {
        let tx = Transaction {
            station: "CP-1".to_owned(),
            id: "123".to_owned(),
            status: "active".to_owned(),
            connector: Some(1),
            meter_start: Some(10),
            energy_wh: Some(110.0),
            ..Transaction::default()
        };
        let document = csv_document(&[tx]);
        let lines: Vec<&str> = document.split("\r\n").collect();
        assert_eq!(lines[0], CSV_HEADER.join(","));
        assert_eq!(lines[1], "CP-1,123,1,,,10,,active,,,,110,,,,100,,");
        assert_eq!(lines[2], "");
    }
}
