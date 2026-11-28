//! The Rules and faults tab: fixed answers to the station's CALLs and injected faults, through
//! the `/v1/rules`, `/v1/stations/{id}/rules` and `/v1/faults` endpoints.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

use crate::api::{FaultList, NewFault, encode};
use crate::commands::json_object;
use crate::store::use_store;
use crate::ui::{Empty, ErrorLine, Section};
use crate::util::clean;

const PROTOCOLS: [&str; 3] = ["ocpp1.6", "ocpp2.0.1", "ocpp2.1"];
const FAULT_KINDS: [&str; 4] = ["call_error", "silence", "close", "invalid_payload"];

#[component]
pub fn RulesTab(id: String) -> impl IntoView {
    view! {
        <div class="rules">
            <p class="muted intro">
                "Rules give a fixed answer to a CALL the station sends. Faults break an answer on purpose. A fault beats a rule, a station rule beats a global one."
            </p>
            <RuleSection station=Some(id.clone())/>
            <RuleSection station=None/>
            <FaultSection station=id/>
        </div>
    }
}

fn protocol_select(value: RwSignal<String>, any_label: &'static str) -> impl IntoView {
    view! {
        <select class="input" prop:value=move || value.get()
            on:change=move |e| value.set(event_target_value(&e))>
            <option value="" selected=move || value.get().is_empty()>{any_label}</option>
            {PROTOCOLS.into_iter().map(|p| view! {
                <option value=p selected=move || value.get() == p>{p}</option>
            }).collect_view()}
        </select>
    }
}

/// Station rules (`station` set) or global rules (`None`).
#[component]
fn RuleSection(station: Option<String>) -> impl IntoView {
    let store = use_store();
    let scope = station.clone();
    let list_path = {
        let scope = scope.clone();
        move || match &scope {
            Some(id) => format!("/v1/stations/{}/rules", encode(id)),
            None => "/v1/rules".to_owned(),
        }
    };
    let rules = crate::store::use_fetch::<Value>(list_path);
    let action = RwSignal::new(String::new());
    let protocol = RwSignal::new(String::new());
    let payload = RwSignal::new("{}".to_owned());
    let error = RwSignal::new(None::<String>);

    let rule_path = {
        let scope = scope.clone();
        move |action: &str, protocol: &str| {
            let base = match &scope {
                Some(id) => format!("/v1/stations/{}/rules/{}", encode(id), encode(action)),
                None => format!("/v1/rules/{}", encode(action)),
            };
            if protocol.is_empty() {
                base
            } else {
                format!("{base}?protocol={}", encode(protocol))
            }
        }
    };
    let mutate = Callback::new(
        move |(method, path, body): (&'static str, String, Option<String>)| {
            spawn_local(async move {
                match store.send(method, &path, body.as_deref()).await {
                    Ok(_) => {
                        error.set(None);
                        rules.reload();
                    }
                    Err(e) => error.set(Some(e.to_string())),
                }
            });
        },
    );
    let set = {
        let rule_path = rule_path.clone();
        move || {
            let name = action.get_untracked();
            let name = name.trim();
            if name.is_empty() {
                error.set(Some("the action is required".to_owned()));
                return;
            }
            match json_object(&payload.get_untracked()) {
                Ok(body) => mutate.run((
                    "PUT",
                    rule_path(name, &protocol.get_untracked()),
                    Some(body.to_string()),
                )),
                Err(e) => error.set(Some(e)),
            }
        }
    };
    let rows = Memo::new(move |_| {
        rules
            .data
            .with(|r| r.as_ref().map(rule_rows).unwrap_or_default())
    });
    let title = if station.is_some() {
        "Rules of this station"
    } else {
        "Global rules"
    };
    let global = station.is_none();
    view! {
        <Section title=title>
            <ErrorLine text=rules.error/>
            <Show when=move || rows.with(Vec::is_empty)>
                <Empty>"No rules."</Empty>
            </Show>
            <ul class="rows">
                <For each=move || rows.get() key=|r| (r.action.clone(), r.protocol.clone()) children=move |row| {
                    let clear_path = rule_path(&row.action, &row.protocol);
                    view! {
                        <li class="row">
                            <span class="mono">{clean(&row.action)}</span>
                            <span class="badge">{if row.protocol.is_empty() { "any protocol".to_owned() } else { row.protocol.clone() }}</span>
                            <code class="row-body">{row.payload.map(|p| clean(&p.to_string()))}</code>
                            <button class="button" on:click=move |_| mutate.run(("DELETE", clear_path.clone(), None))>"Clear"</button>
                        </li>
                    }
                }/>
            </ul>
            <form class="inline-form" on:submit=move |e| { e.prevent_default(); set(); }>
                <label class="field"><span class="field-label">"Action"</span>
                    <input class="input" type="text" placeholder="e.g. Heartbeat" autocomplete="off"
                        prop:value=move || action.get() on:input=move |e| action.set(event_target_value(&e))/>
                </label>
                <label class="field"><span class="field-label">"Protocol"</span>{protocol_select(protocol, "any")}</label>
                <label class="field wide"><span class="field-label">"Answer payload (JSON object)"</span>
                    <textarea class="input mono" rows="3" spellcheck="false"
                        prop:value=move || payload.get() on:input=move |e| payload.set(event_target_value(&e))></textarea>
                </label>
                <div class="form-actions"><button class="button primary" type="submit">"Set rule"</button></div>
            </form>
            <ErrorLine text=error/>
            {global.then(|| view! { <p class="hint">"A global rule applies to every station that has no rule of its own for the action."</p> })}
        </Section>
    }
}

/// One rule row of either list.
#[derive(Debug, Clone, PartialEq)]
struct RuleRow {
    action: String,
    /// Empty when the rule applies to every protocol.
    protocol: String,
    payload: Option<Value>,
}

#[component]
fn FaultSection(station: String) -> impl IntoView {
    let store = use_store();
    let faults = crate::store::use_fetch::<FaultList>(|| "/v1/faults".to_owned());
    let action = RwSignal::new(String::new());
    let kind = RwSignal::new("call_error".to_owned());
    let times = RwSignal::new("1".to_owned());
    let this_station = RwSignal::new(true);
    let protocol = RwSignal::new(String::new());
    let payload = RwSignal::new("{}".to_owned());
    let error = RwSignal::new(None::<String>);
    let mutate = Callback::new(
        move |(method, path, body): (&'static str, String, Option<String>)| {
            spawn_local(async move {
                match store.send(method, &path, body.as_deref()).await {
                    Ok(_) => {
                        error.set(None);
                        faults.reload();
                    }
                    Err(e) => error.set(Some(e.to_string())),
                }
            });
        },
    );
    let scope_station = station.clone();
    let inject = move || {
        let name = action.get_untracked();
        let name = name.trim();
        if name.is_empty() {
            error.set(Some("the action is required".to_owned()));
            return;
        }
        let Ok(count) = times.get_untracked().trim().parse::<u32>() else {
            error.set(Some("times is a whole number from 1".to_owned()));
            return;
        };
        let kind = kind.get_untracked();
        let body = if kind == "invalid_payload" {
            match json_object(&payload.get_untracked()) {
                Ok(value) => Some(value),
                Err(e) => {
                    error.set(Some(e));
                    return;
                }
            }
        } else {
            None
        };
        let protocol = protocol.get_untracked();
        let fault = NewFault {
            action: name.to_owned(),
            kind,
            times: count,
            payload: body,
            station: this_station.get_untracked().then(|| scope_station.clone()),
            protocol: (!protocol.is_empty()).then_some(protocol),
        };
        match serde_json::to_string(&fault) {
            Ok(text) => mutate.run(("POST", "/v1/faults".to_owned(), Some(text))),
            Err(e) => error.set(Some(e.to_string())),
        }
    };
    let rows = Memo::new(move |_| {
        faults
            .data
            .with(|f| f.as_ref().map(|f| f.faults.clone()).unwrap_or_default())
    });
    let mine = station.clone();
    view! {
        <Section title="Faults">
            <ErrorLine text=faults.error/>
            <Show when=move || rows.with(Vec::is_empty)>
                <Empty>"No fault is waiting."</Empty>
            </Show>
            <ul class="rows">
                <For each=move || rows.get() key=|f| (f.id.clone(), f.remaining) children=move |fault| {
                    let remove = format!("/v1/faults/{}", encode(&fault.id));
                    let here = fault.station.as_deref() == Some(mine.as_str());
                    view! {
                        <li class="row">
                            <span class="mono">{clean(&fault.action)}</span>
                            <span class="badge">{fault.kind.clone()}</span>
                            <span class="badge">{match (&fault.station, &fault.protocol) {
                                (Some(_), _) if here => "this station".to_owned(),
                                (Some(other), _) => format!("station {}", clean(other)),
                                (None, Some(p)) => format!("all stations, {p}"),
                                (None, None) => "all stations".to_owned(),
                            }}</span>
                            <span class="row-body muted">{format!("{} left", fault.remaining)}</span>
                            <button class="button" on:click=move |_| mutate.run(("DELETE", remove.clone(), None))>"Remove"</button>
                        </li>
                    }
                }/>
            </ul>
            <form class="inline-form" on:submit=move |e| { e.prevent_default(); inject(); }>
                <label class="field"><span class="field-label">"Action"</span>
                    <input class="input" type="text" placeholder="e.g. Heartbeat" autocomplete="off"
                        prop:value=move || action.get() on:input=move |e| action.set(event_target_value(&e))/>
                </label>
                <label class="field"><span class="field-label">"Kind"</span>
                    <select class="input" prop:value=move || kind.get() on:change=move |e| kind.set(event_target_value(&e))>
                        {FAULT_KINDS.into_iter().map(|k| view! { <option value=k selected=move || kind.get() == k>{k}</option> }).collect_view()}
                    </select>
                </label>
                <label class="field"><span class="field-label">"Times"</span>
                    <input class="input" type="text" inputmode="numeric" autocomplete="off"
                        prop:value=move || times.get() on:input=move |e| times.set(event_target_value(&e))/>
                </label>
                <label class="field"><span class="field-label">"Scope"</span>
                    <select class="input" prop:value=move || if this_station.get() { "this" } else { "all" }
                        on:change=move |e| this_station.set(event_target_value(&e) == "this")>
                        <option value="this" selected=move || this_station.get()>"this station"</option>
                        <option value="all" selected=move || !this_station.get()>"all stations"</option>
                    </select>
                </label>
                <label class="field"><span class="field-label">"Protocol"</span>{protocol_select(protocol, "any")}</label>
                <Show when=move || kind.get() == "invalid_payload">
                    <label class="field wide"><span class="field-label">"Payload sent as the answer (JSON object)"</span>
                        <textarea class="input mono" rows="3" spellcheck="false"
                            prop:value=move || payload.get() on:input=move |e| payload.set(event_target_value(&e))></textarea>
                    </label>
                </Show>
                <div class="form-actions">
                    <button class="button primary" type="submit">"Inject fault"</button>
                    <button class="button danger" type="button"
                        on:click=move |_| mutate.run(("DELETE", "/v1/faults".to_owned(), None))>"Clear all faults"</button>
                </div>
            </form>
            <ErrorLine text=error/>
        </Section>
    }
}

/// The rows of either list answer: `{"rules":["Heartbeat"],"protocol_rules":[{action,
/// protocol}]}` for the global list, `{"station":..,"rules":[{action,protocol,payload}]}` for
/// one station.
fn rule_rows(list: &Value) -> Vec<RuleRow> {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let mut rows = Vec::new();
    for rule in list["rules"].as_array().into_iter().flatten() {
        rows.push(match rule {
            Value::String(action) => RuleRow {
                action: action.clone(),
                protocol: String::new(),
                payload: None,
            },
            other => RuleRow {
                action: text(&other["action"]),
                protocol: text(&other["protocol"]),
                payload: Some(other["payload"].clone()),
            },
        });
    }
    for rule in list["protocol_rules"].as_array().into_iter().flatten() {
        rows.push(RuleRow {
            action: text(&rule["action"]),
            protocol: text(&rule["protocol"]),
            payload: None,
        });
    }
    rows
}
