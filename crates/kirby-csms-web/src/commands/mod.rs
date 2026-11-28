//! The Commands tab: the action list of the station's protocol, generated forms and the
//! answers of the calls sent from them. The form rules mirror the terminal UI (see `form`).

mod form;

use std::sync::atomic::{AtomicU32, Ordering};

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

use crate::api::{ActionList, ActionSpec, ActiveTx, FieldSpec, encode};
use crate::store::use_store;
use crate::ui::{Empty, ErrorLine};
use crate::util::{clean, clock_time, now_rfc3339, pretty};
pub use form::json_object;
use form::{ANY, build_payload, field_value};

/// The key of the "custom call" entry of the list.
const CUSTOM: &str = "\u{0}custom";
/// Answers kept in the panel.
const OUTCOME_CAP: usize = 20;

static REQUEST_ID: AtomicU32 = AtomicU32::new(0);

/// A fresh request id for the fields that take one (`remoteStartId`, `requestId`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn next_request_id() -> u32 {
    // Different on every page load, small enough for a 32-bit signed field.
    let start = (js_sys::Date::now() / 1000.0) as u64 % 1_000_000 + 1;
    let _ = REQUEST_ID.compare_exchange(0, start as u32, Ordering::Relaxed, Ordering::Relaxed);
    REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// Normalizes a valid time to UTC whole seconds, like the terminal UI.
fn utc_seconds(text: &str) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(text));
    if date.get_time().is_nan() {
        return text.to_owned();
    }
    let iso = String::from(date.to_iso_string());
    match iso.split_once('.') {
        Some((head, _)) => format!("{head}Z"),
        None => iso,
    }
}

/// What a form field starts with.
fn initial_value(spec: &FieldSpec) -> String {
    match spec.ty.as_str() {
        "choice" | "boolean" => {
            let first = if spec.ty == "boolean" {
                Some("true".to_owned())
            } else {
                spec.choices.first().cloned()
            };
            spec.default
                .clone()
                .or(if spec.required { first } else { None })
                .unwrap_or_else(|| ANY.to_owned())
        }
        "transaction" => String::new(),
        _ => match spec.name.as_str() {
            "remoteStartId" | "requestId" => next_request_id().to_string(),
            "timestamp" if spec.ty == "timestamp" => now_rfc3339(),
            _ => spec.default.clone().unwrap_or_default(),
        },
    }
}

/// One answer shown in the outcome panel.
#[derive(Debug, Clone, PartialEq)]
struct Outcome {
    seq: u64,
    action: String,
    at: String,
    /// `result`, `call_error`, `timeout` or `refused` (the API did not deliver the call).
    kind: String,
    body: String,
}

#[component]
pub fn CommandsTab(id: String) -> impl IntoView {
    let store = use_store();
    let station = {
        let id = id.clone();
        Memo::new(move |_| store.station(&id))
    };
    let protocol = Memo::new(move |_| {
        station
            .get()
            .and_then(|s| s.protocol)
            .unwrap_or_else(|| "ocpp1.6".to_owned())
    });
    let table = RwSignal::new(None::<ActionList>);
    let table_error = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let protocol = protocol.get();
        spawn_local(async move {
            let path = format!("/v1/actions?protocol={}", encode(&protocol));
            match store.get::<ActionList>(&path).await {
                Ok(list) => {
                    table.set(Some(list));
                    table_error.set(None);
                }
                Err(e) => table_error.set(Some(e.to_string())),
            }
        });
    });

    let search = RwSignal::new(String::new());
    let selected = RwSignal::new(None::<String>);
    let outcomes = RwSignal::new(Vec::<Outcome>::new());
    let seq = StoredValue::new(0_u64);

    let groups = Memo::new(move |_| {
        let needle = search.get().trim().to_lowercase();
        let Some(list) = table.get() else {
            return Vec::new();
        };
        let mut groups: Vec<(String, Vec<ActionSpec>)> = Vec::new();
        for action in list.actions {
            let hit = needle.is_empty()
                || action.name.to_lowercase().contains(&needle)
                || action.title.to_lowercase().contains(&needle)
                || action.summary.to_lowercase().contains(&needle);
            if !hit {
                continue;
            }
            match groups.iter_mut().find(|(group, _)| *group == action.group) {
                Some((_, items)) => items.push(action),
                None => groups.push((action.group.clone(), vec![action])),
            }
        }
        groups
    });

    // The call itself: sends, then records the answer for the panel.
    let send = {
        let id = id.clone();
        Callback::new(move |(action, payload): (String, Value)| {
            let id = id.clone();
            let path = format!("/v1/stations/{}/calls", encode(&id));
            let body = json!({"action": action, "payload": payload}).to_string();
            spawn_local(async move {
                let result = store.send("POST", &path, Some(&body)).await;
                let (kind, text) = match result {
                    Ok(text) => {
                        let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                        let kind = answer["outcome"].as_str().unwrap_or("result").to_owned();
                        (kind, pretty(&answer))
                    }
                    Err(e) => ("refused".to_owned(), e.to_string()),
                };
                let n = seq.get_value();
                seq.set_value(n + 1);
                outcomes.update(|list| {
                    list.insert(
                        0,
                        Outcome {
                            seq: n,
                            action,
                            at: clock_time(&now_rfc3339()),
                            kind,
                            body: text,
                        },
                    );
                    list.truncate(OUTCOME_CAP);
                });
            });
        })
    };

    let form_station = id.clone();
    view! {
        <div class="commands">
            <ErrorLine text=table_error/>
            <aside class="action-list" aria-label="Actions">
                <input class="input" type="search" placeholder="Search actions" aria-label="Search actions"
                    autocomplete="off"
                    prop:value=move || search.get()
                    on:input=move |e| search.set(event_target_value(&e))/>
                <div class="action-scroll">
                    <button
                        class="action"
                        class:selected=move || selected.get().as_deref() == Some(CUSTOM)
                        on:click=move |_| selected.set(Some(CUSTOM.to_owned()))
                    >
                        <span class="action-title">"Custom call"</span>
                        <span class="action-summary">"Any action name with a JSON payload"</span>
                    </button>
                    <Show when=move || groups.with(Vec::is_empty) && table.get().is_some()>
                        <Empty>"No action matches."</Empty>
                    </Show>
                    <For each=move || groups.get() key=|(group, items)| (group.clone(), items.len()) children=move |(group, items)| {
                        view! {
                            <h4 class="action-group">{if group == "security" { "Security".to_owned() } else { "Core".to_owned() }}</h4>
                            {items.into_iter().map(|action| {
                                let name = action.name.clone();
                                let pick = action.name.clone();
                                view! {
                                    <button
                                        class="action"
                                        class:selected=move || selected.get().as_deref() == Some(name.as_str())
                                        on:click=move |_| selected.set(Some(pick.clone()))
                                    >
                                        <span class="action-title">{action.title.clone()}</span>
                                        {(!same_words(&action.title, &action.name)).then(|| view! {
                                            <span class="action-name">{action.name.clone()}</span>
                                        })}
                                    </button>
                                }
                            }).collect_view()}
                        }
                    }/>
                </div>
            </aside>
            <div class="action-panel">
                {move || {
                    let station_id = form_station.clone();
                    match selected.get() {
                        None => view! {
                            <Empty>"Pick an action to build a request."</Empty>
                        }.into_any(),
                        Some(name) if name == CUSTOM => view! {
                            <CustomForm send=send/>
                        }.into_any(),
                        Some(name) => {
                            let spec = table.with(|t| t.as_ref().and_then(|t| t.actions.iter().find(|a| a.name == name).cloned()));
                            match spec {
                                Some(spec) => view! {
                                    <ActionForm station=station_id spec=spec send=send/>
                                }.into_any(),
                                None => view! { <Empty>"This action is not in the table of the station's protocol."</Empty> }.into_any(),
                            }
                        }
                    }
                }}
                <Outcomes outcomes=outcomes/>
            </div>
        </div>
    }
}

#[component]
fn Outcomes(outcomes: RwSignal<Vec<Outcome>>) -> impl IntoView {
    view! {
        <section class="outcomes" aria-live="polite">
            <header class="section-head">
                <h3>"Answers"</h3>
                <button class="button" on:click=move |_| outcomes.set(Vec::new())
                    disabled=move || outcomes.with(Vec::is_empty)>"Clear"</button>
            </header>
            <Show when=move || outcomes.with(Vec::is_empty)>
                <Empty>"The answer of each call appears here."</Empty>
            </Show>
            <For each=move || outcomes.get() key=|o| o.seq children=move |o| {
                let chip = match o.kind.as_str() {
                    "result" => "chip chip-ok",
                    "refused" | "call_error" => "chip chip-bad",
                    _ => "chip chip-info",
                };
                view! {
                    <article class="outcome">
                        <header>
                            <span class=chip>{o.kind.clone()}</span>
                            <strong>{clean(&o.action)}</strong>
                            <span class="muted">{o.at.clone()}</span>
                        </header>
                        <pre class="json">{o.body.clone()}</pre>
                    </article>
                }
            }/>
        </section>
    }
}

/// Fires `submit` on Ctrl+Enter or Cmd+Enter inside a textarea (Enter alone adds a line).
fn submit_on_ctrl_enter(event: &leptos::ev::KeyboardEvent, submit: impl Fn()) {
    if event.key() == "Enter" && (event.ctrl_key() || event.meta_key()) {
        event.prevent_default();
        submit();
    }
}

#[component]
fn CustomForm(send: Callback<(String, Value)>) -> impl IntoView {
    let action = RwSignal::new(String::new());
    let payload = RwSignal::new("{}".to_owned());
    let error = RwSignal::new(None::<String>);
    let submit = move || {
        let name = action.get_untracked();
        let name = name.trim();
        if name.is_empty() {
            error.set(Some("the action name is required".to_owned()));
            return;
        }
        match json_object(&payload.get_untracked()) {
            Ok(value) => {
                error.set(None);
                send.run((name.to_owned(), value));
            }
            Err(e) => error.set(Some(e)),
        }
    };
    view! {
        <form class="action-form" on:submit=move |e| { e.prevent_default(); submit(); }>
            <h3>"Custom call"</h3>
            <p class="muted">"Sends any action with the payload as written. The station answers with a CALLERROR when it does not know the action."</p>
            <label class="field">
                <span class="field-label">"Action"</span>
                <input class="input" type="text" autocomplete="off" placeholder="e.g. GetConfiguration"
                    prop:value=move || action.get()
                    on:input=move |e| action.set(event_target_value(&e))/>
            </label>
            <label class="field">
                <span class="field-label">"Payload (JSON)"</span>
                <textarea class="input mono" rows="8" spellcheck="false"
                    prop:value=move || payload.get()
                    on:input=move |e| payload.set(event_target_value(&e))
                    on:keydown=move |e| submit_on_ctrl_enter(&e, submit)></textarea>
                <span class="hint">"Empty sends {}. Ctrl+Enter sends."</span>
            </label>
            <ErrorLine text=error/>
            <div class="form-actions"><button class="button primary" type="submit">"Send"</button></div>
        </form>
    }
}

/// One field of a generated form.
#[derive(Clone)]
struct FieldState {
    spec: FieldSpec,
    value: RwSignal<String>,
    touched: RwSignal<bool>,
}

/// Whether a title only repeats the action name ("Reset" and `Reset`): then the name is not shown twice.
fn same_words(title: &str, name: &str) -> bool {
    let letters = |text: &str| {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    letters(title) == letters(name)
}

fn short_tx(tx: &ActiveTx) -> String {
    let id: String = clean(&tx.id).chars().take(12).collect();
    let part = |n: Option<u32>| n.map_or_else(|| "-".to_owned(), |n| n.to_string());
    format!("{id}  {}/{}", part(tx.evse), part(tx.connector))
}

#[component]
fn ActionForm(station: String, spec: ActionSpec, send: Callback<(String, Value)>) -> impl IntoView {
    let store = use_store();
    let txs = Memo::new(move |_| {
        store
            .station(&station)
            .map_or_default(|s| s.active_transactions)
    });
    let raw = spec.input == "raw";
    let template = if raw {
        spec.template
            .as_ref()
            .map_or_else(|| "{}".to_owned(), pretty)
    } else {
        String::new()
    };
    let raw_text = RwSignal::new(template);
    let fields: Vec<FieldState> = spec
        .fields
        .iter()
        .map(|field| FieldState {
            spec: field.clone(),
            value: RwSignal::new(initial_value(field)),
            touched: RwSignal::new(false),
        })
        .collect();
    let form_error = RwSignal::new(None::<String>);
    let name = spec.name.clone();

    let submit = {
        let fields = fields.clone();
        let name = name.clone();
        move || {
            let payload = if raw {
                json_object(&raw_text.get_untracked())
            } else {
                for field in &fields {
                    field.touched.set(true);
                }
                let entries: Vec<(FieldSpec, String)> = fields
                    .iter()
                    .map(|f| (f.spec.clone(), f.value.get_untracked()))
                    .collect();
                build_payload(&entries, &utc_seconds)
            };
            match payload {
                Ok(payload) => {
                    form_error.set(None);
                    send.run((name.clone(), payload));
                }
                Err(e) => form_error.set(Some(e)),
            }
        }
    };
    let submit_key = submit.clone();
    view! {
        <form class="action-form" on:submit=move |e| { e.prevent_default(); submit(); }>
            <h3>{spec.title.clone()}</h3>
            <p class="muted">{spec.summary.clone()}</p>
            {(!same_words(&spec.title, &spec.name)).then(|| view! {
                <p class="mono action-code">{clean(&spec.name)}</p>
            })}
            {if spec.input == "none" {
                view! { <p class="muted">"This action takes no input."</p> }.into_any()
            } else if raw {
                view! {
                    <label class="field">
                        <span class="field-label">"Payload (JSON)"</span>
                        <textarea class="input mono" rows="12" spellcheck="false"
                            prop:value=move || raw_text.get()
                            on:input=move |e| raw_text.set(event_target_value(&e))
                            on:keydown=move |e| submit_on_ctrl_enter(&e, submit_key.clone())></textarea>
                        <span class="hint">"Empty sends {}. Ctrl+Enter sends."</span>
                    </label>
                }.into_any()
            } else {
                fields.iter().cloned().map(|field| view! { <FieldInput field=field txs=txs/> }).collect_view().into_any()
            }}
            <ErrorLine text=form_error/>
            <div class="form-actions">
                <button class="button primary" type="submit"
                    disabled=move || fields.iter().any(|f| f.spec.ty == "transaction" && f.spec.required && txs.with(Vec::is_empty))>
                    "Send"
                </button>
            </div>
        </form>
    }
}

#[component]
fn FieldInput(field: FieldState, txs: Memo<Vec<ActiveTx>>) -> impl IntoView {
    let FieldState {
        spec,
        value,
        touched,
    } = field;
    // A transaction field starts on the first running transaction and follows the list.
    if spec.ty == "transaction" {
        Effect::new(move |_| {
            let list = txs.get();
            let current = value.get_untracked();
            if !list.iter().any(|t| t.id == current) {
                value.set(if spec.required {
                    list.first().map_or_default(|t| t.id.clone())
                } else {
                    String::new()
                });
            }
        });
    }
    let problem = {
        let spec = spec.clone();
        Memo::new(move |_| {
            let text = value.get();
            if !touched.get() {
                return None;
            }
            field_value(&spec, &text, &|t| t.to_owned()).err()
        })
    };
    let mark = move || touched.set(true);
    let optional_any = (!spec.required).then(
        || view! { <option value="" selected=move || value.get().is_empty()>"(any)"</option> },
    );
    let control = match spec.ty.as_str() {
        "choice" | "boolean" => {
            let choices: Vec<String> = if spec.ty == "boolean" {
                vec!["true".to_owned(), "false".to_owned()]
            } else {
                spec.choices.clone()
            };
            view! {
                <select class="input" prop:value=move || value.get()
                    on:change=move |e| { value.set(event_target_value(&e)); mark(); }>
                    {optional_any}
                    {choices.into_iter().map(|c| {
                        let current = c.clone();
                        let label = c.clone();
                        view! { <option value=c selected=move || value.get() == current>{label}</option> }
                    }).collect_view()}
                </select>
            }.into_any()
        }
        "transaction" => view! {
            <select class="input" prop:value=move || value.get() disabled=move || txs.with(Vec::is_empty)
                on:change=move |e| { value.set(event_target_value(&e)); mark(); }>
                {optional_any}
                {move || {
                    let list = txs.get();
                    if list.is_empty() {
                        view! { <option value="">"no active transaction"</option> }.into_any()
                    } else {
                        list.into_iter().map(|tx| {
                            let id = tx.id.clone();
                            view! { <option value=tx.id.clone() selected=move || value.get() == id>{short_tx(&tx)}</option> }
                        }).collect_view().into_any()
                    }
                }}
            </select>
        }.into_any(),
        "json" | "pem" => view! {
            <textarea class="input mono" rows="4" spellcheck="false"
                prop:value=move || value.get()
                on:input=move |e| { value.set(event_target_value(&e)); mark(); }></textarea>
        }.into_any(),
        other => {
            let numeric = other == "integer";
            view! {
                <input class="input" type="text" autocomplete="off" spellcheck="false"
                    inputmode=if numeric { "numeric" } else { "text" }
                    prop:value=move || value.get()
                    on:input=move |e| { value.set(event_target_value(&e)); mark(); }/>
            }.into_any()
        }
    };
    view! {
        <label class="field">
            <span class="field-label">
                {spec.label.clone()}
                {spec.required.then(|| view! { <span class="req" title="required">" *"</span> })}
            </span>
            {control}
            {(!spec.hint.is_empty()).then(|| view! { <span class="hint">{spec.hint.clone()}</span> })}
            <span class="field-error" role="alert">{move || problem.get().unwrap_or_default()}</span>
        </label>
    }
}
