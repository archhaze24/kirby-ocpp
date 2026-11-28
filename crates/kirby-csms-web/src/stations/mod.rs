//! The station sidebar and the page of one station with its tabs.

mod overview;

use leptos::prelude::*;

use crate::api::{Station, encode};
use crate::commands::CommandsTab;
use crate::log::LogTab;
use crate::rules::RulesTab;
use crate::store::{Tab, use_store};
use crate::ui::{Badge, Dot, protocol_label};
use crate::util::clean;
use overview::OverviewTab;

fn matches_filter(station: &Station, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return true;
    }
    [
        Some(station.id.as_str()),
        station.vendor.as_deref(),
        station.model.as_deref(),
        station.protocol.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.to_lowercase().contains(&filter))
}

#[component]
pub fn Sidebar() -> impl IntoView {
    let store = use_store();
    let filter = RwSignal::new(String::new());
    let shown = Memo::new(move |_| {
        let text = filter.get();
        store.stations.with(|stations| {
            stations
                .iter()
                .filter(|station| matches_filter(station, &text))
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    view! {
        <aside class="sidebar" aria-label="Stations">
            <div class="sidebar-head">
                <input
                    id="station-filter"
                    class="input"
                    type="search"
                    placeholder="Filter stations   ( / )"
                    aria-label="Filter stations"
                    autocomplete="off"
                    prop:value=move || filter.get()
                    on:input=move |event| filter.set(event_target_value(&event))
                    on:keydown=move |event| {
                        if event.key() == "Escape" {
                            filter.set(String::new());
                        }
                    }
                />
            </div>
            <ul class="station-list">
                <For
                    each=move || shown.get()
                    key=|station| station.id.clone()
                    children=move |station| {
                        let id = station.id.clone();
                        let current = {
                            let id = id.clone();
                            move || store.station(&id).unwrap_or_default()
                        };
                        let select_id = id.clone();
                        let active_id = id.clone();
                        view! {
                            <li>
                                <button
                                    class="station-item"
                                    class:selected=move || store.selected.get().as_deref() == Some(active_id.as_str())
                                    on:click=move |_| {
                                        store.selected.set(Some(select_id.clone()));
                                        store.view.set(crate::store::View::Stations);
                                        store.drawer.set(false);
                                    }
                                >
                                    <Dot
                                        class=Signal::derive({
                                            let current = current.clone();
                                            move || if current().connected { "ok" } else { "off" }
                                        })
                                        title="connection"
                                    />
                                    <span class="station-main">
                                        <span class="station-id">{clean(&id)}</span>
                                        <span class="station-sub">{
                                            let current = current.clone();
                                            move || {
                                                let station = current();
                                                match (station.vendor, station.model) {
                                                    (Some(vendor), Some(model)) => clean(&format!("{vendor} {model}")),
                                                    (Some(only), None) | (None, Some(only)) => clean(&only),
                                                    (None, None) => if station.connected { "booting".to_owned() } else { "offline".to_owned() },
                                                }
                                            }
                                        }</span>
                                    </span>
                                    <span class="station-meta">
                                        <Badge text=Signal::derive({
                                            let current = current.clone();
                                            move || protocol_label(current().protocol.as_deref())
                                        })/>
                                        {
                                            let current = current.clone();
                                            move || {
                                                let count = current().active_transactions.len();
                                                (count > 0).then(|| view! {
                                                    <span class="badge tx" title="active transactions">{format!("{count} tx")}</span>
                                                })
                                            }
                                        }
                                    </span>
                                </button>
                            </li>
                        }
                    }
                />
            </ul>
            <Show when=move || shown.with(Vec::is_empty)>
                <p class="empty">
                    {move || if store.stations.with(Vec::is_empty) { "No stations yet." } else { "No station matches." }}
                </p>
            </Show>
        </aside>
    }
}

#[component]
pub fn StationPage(id: String) -> impl IntoView {
    let store = use_store();
    let station = {
        let id = id.clone();
        Memo::new(move |_| store.station(&id))
    };
    let tabs = [
        (Tab::Overview, "Overview"),
        (Tab::Log, "Log"),
        (Tab::Commands, "Commands"),
        (Tab::Rules, "Rules and faults"),
    ];
    let tab_id = id.clone();
    let forget_error = RwSignal::new(None::<String>);
    let forget = {
        let id = id.clone();
        move |_| {
            let id = id.clone();
            leptos::task::spawn_local(async move {
                let path = format!("/v1/stations/{}", encode(&id));
                match store.send("DELETE", &path, None).await {
                    Ok(_) => {
                        store.selected.set(None);
                        store.bump();
                    }
                    Err(error) => forget_error.set(Some(error.to_string())),
                }
            });
        }
    };
    view! {
        <div class="station-page">
            <header class="page-head">
                <h2 class="page-title">
                    <Dot class=Signal::derive(move || if station.get().is_some_and(|s| s.connected) { "ok" } else { "off" })/>
                    {clean(&id)}
                </h2>
                <Badge text=Signal::derive(move || {
                    format!("OCPP {}", protocol_label(station.get().and_then(|s| s.protocol).as_deref()))
                })/>
                <span class="muted">{move || match station.get() {
                    Some(s) if s.connected => format!("connected from {}", s.remote_addr.unwrap_or_else(|| "unknown".to_owned())),
                    Some(_) => "disconnected".to_owned(),
                    None => "unknown station".to_owned(),
                }}</span>
                <span class="spacer"></span>
                <Show when=move || station.get().is_some_and(|s| !s.connected)>
                    <button class="button danger" title="Forget this disconnected station" on:click=forget.clone()>"Forget"</button>
                </Show>
            </header>
            <crate::ui::ErrorLine text=forget_error/>
            <div class="tabs" role="tablist">
                {tabs
                    .into_iter()
                    .map(|(tab, label)| {
                        view! {
                            <button
                                class="tab"
                                role="tab"
                                class:active=move || store.tab.get() == tab
                                aria-selected=move || (store.tab.get() == tab).to_string()
                                on:click=move |_| store.tab.set(tab)
                            >{label}</button>
                        }
                    })
                    .collect_view()}
            </div>
            <div class="tab-body" role="tabpanel">
                {move || {
                    let id = tab_id.clone();
                    match store.tab.get() {
                        Tab::Overview => view! { <OverviewTab id=id/> }.into_any(),
                        Tab::Log => view! { <LogTab id=id/> }.into_any(),
                        Tab::Commands => view! { <CommandsTab id=id/> }.into_any(),
                        Tab::Rules => view! { <RulesTab id=id/> }.into_any(),
                    }
                }}
            </div>
        </div>
    }
}
