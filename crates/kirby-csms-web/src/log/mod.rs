//! The Log tab: live frames of one station with filters, pause, follow and a JSON detail.

use std::collections::HashMap;

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{Frame, History, encode};
use crate::store::{LogFrame, use_store};
use crate::ui::{Empty, ErrorLine};
use crate::util::{clean, clock_time, pretty};

/// Rows drawn at most; older frames stay in memory and in the filter's reach.
const ROW_CAP: usize = 500;

/// The action a frame belongs to: its own, or for a reply the action of the CALL it answers.
fn action_of<'a>(frame: &'a Frame, calls: &'a HashMap<String, String>) -> Option<&'a str> {
    frame.action.as_deref().or_else(|| {
        frame
            .id
            .as_ref()
            .and_then(|id| calls.get(id))
            .map(String::as_str)
    })
}

/// One line of text that sums a frame up.
fn summary(frame: &Frame) -> String {
    let compact = |value: &serde_json::Value| serde_json::to_string(value).unwrap_or_default();
    let text = match frame.kind.as_str() {
        "error" | "result_error" => format!(
            "{} {}",
            frame.code.as_deref().unwrap_or_default(),
            frame.description.as_deref().unwrap_or_default()
        ),
        _ => frame.payload.as_ref().map(compact).unwrap_or_default(),
    };
    clean(&text)
}

#[component]
pub fn LogTab(id: String) -> impl IntoView {
    let store = use_store();
    let error = RwSignal::new(None::<String>);
    // Earlier frames come from the history; live ones are already in the ring.
    {
        let id = id.clone();
        spawn_local(async move {
            let path = format!("/v1/stations/{}/history?limit=300", encode(&id));
            match store.get::<History>(&path).await {
                Ok(history) => store.merge_history(&id, history.frames),
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    }

    let direction = RwSignal::new(String::new());
    let kind = RwSignal::new(String::new());
    let action_filter = RwSignal::new(String::new());
    let paused = RwSignal::new(None::<u64>);
    let cleared = RwSignal::new(0_u64);
    let follow = RwSignal::new(true);
    let selected = RwSignal::new(None::<u64>);
    let list_ref = NodeRef::<html::Div>::new();

    let station_id = id.clone();
    let matching = Memo::new(move |_| {
        let direction = direction.get();
        let kind = kind.get();
        let needle = action_filter.get().trim().to_lowercase();
        let freeze = paused.get();
        let floor = cleared.get();
        store.frames.with(|frames| {
            let mine: Vec<&LogFrame> = frames
                .iter()
                .filter(|f| f.frame.station == station_id && f.seq >= floor)
                .collect();
            let calls: HashMap<String, String> = mine
                .iter()
                .filter_map(|f| Some((f.frame.id.clone()?, f.frame.action.clone()?)))
                .collect();
            mine.into_iter()
                .filter(|f| freeze.is_none_or(|top| f.seq <= top))
                .filter(|f| direction.is_empty() || f.frame.direction == direction)
                .filter(|f| kind.is_empty() || f.frame.kind == kind)
                .filter(|f| {
                    needle.is_empty()
                        || action_of(&f.frame, &calls)
                            .is_some_and(|a| a.to_lowercase().contains(&needle))
                })
                .map(|f| LogFrame {
                    seq: f.seq,
                    frame: Frame {
                        action: action_of(&f.frame, &calls).map(str::to_owned),
                        ..f.frame.clone()
                    },
                })
                .collect::<Vec<_>>()
        })
    });
    let rows = Memo::new(move |_| {
        matching.with(|all| {
            let skip = all.len().saturating_sub(ROW_CAP);
            all[skip..].to_vec()
        })
    });
    let detail = Memo::new(move |_| {
        let seq = selected.get()?;
        store.frames.with(|frames| {
            frames
                .iter()
                .find(|f| f.seq == seq)
                .map(|f| pretty(&serde_json::to_value(&f.frame).unwrap_or_default()))
        })
    });

    // Follow: keep the newest row in view after the list changed.
    Effect::new(move |_| {
        rows.track();
        if follow.get_untracked() {
            request_animation_frame(move || {
                if let Some(list) = list_ref.get() {
                    list.set_scroll_top(list.scroll_height());
                }
            });
        }
    });
    window_event_listener(ev::keydown, move |event| {
        if event.key() == "Escape" && selected.get_untracked().is_some() {
            selected.set(None);
        }
    });

    let toggle_pause = move |_| {
        if paused.get_untracked().is_some() {
            paused.set(None);
        } else {
            let top = store
                .frames
                .with_untracked(|f| f.back().map_or(0, |f| f.seq));
            paused.set(Some(top));
        }
    };
    let clear = move |_| {
        let next = store
            .frames
            .with_untracked(|f| f.back().map_or(0, |f| f.seq + 1));
        cleared.set(next);
        selected.set(None);
    };

    view! {
        <div class="log">
            <div class="toolbar">
                <label class="field-inline">
                    <span>"Direction"</span>
                    <select class="input" prop:value=move || direction.get()
                        on:change=move |e| direction.set(event_target_value(&e))>
                        <option value="">"all"</option>
                        <option value="in">"from station"</option>
                        <option value="out">"to station"</option>
                    </select>
                </label>
                <label class="field-inline">
                    <span>"Kind"</span>
                    <select class="input" prop:value=move || kind.get()
                        on:change=move |e| kind.set(event_target_value(&e))>
                        <option value="">"all"</option>
                        <option value="call">"call"</option>
                        <option value="result">"result"</option>
                        <option value="error">"error"</option>
                        <option value="send">"send"</option>
                        <option value="result_error">"result_error"</option>
                    </select>
                </label>
                <label class="field-inline grow">
                    <span>"Action"</span>
                    <input class="input" type="search" placeholder="e.g. MeterValues" autocomplete="off"
                        prop:value=move || action_filter.get()
                        on:input=move |e| action_filter.set(event_target_value(&e))/>
                </label>
                <button class="button" aria-pressed=move || paused.get().is_some().to_string() on:click=toggle_pause>
                    {move || if paused.get().is_some() { "Resume" } else { "Pause" }}
                </button>
                <label class="check">
                    <input type="checkbox" prop:checked=move || follow.get()
                        on:change=move |e| follow.set(event_target_checked(&e))/>
                    <span>"Follow"</span>
                </label>
                <button class="button" on:click=clear>"Clear"</button>
                <span class="muted count-note">{move || {
                    let total = matching.with(Vec::len);
                    if total > ROW_CAP { format!("last {ROW_CAP} of {total}") } else { format!("{total} frames") }
                }}</span>
            </div>
            <ErrorLine text=error/>
            <div class="log-body" class:with-detail=move || detail.get().is_some()>
                <div class="log-list" node_ref=list_ref role="log" aria-label="Frames">
                    <Show when=move || rows.with(Vec::is_empty)>
                        <Empty>"No frames match."</Empty>
                    </Show>
                    <For each=move || rows.get() key=|row| row.seq children=move |row| {
                        let seq = row.seq;
                        let direction_mark = if row.frame.direction == "in" { "←" } else { "→" };
                        let direction_title = if row.frame.direction == "in" { "from the station" } else { "to the station" };
                        let kind_class = format!("kind kind-{}", row.frame.kind);
                        view! {
                            <button
                                class="frame"
                                class:selected=move || selected.get() == Some(seq)
                                on:click=move |_| selected.set(Some(seq))
                            >
                                <span class="frame-time">{clock_time(&row.frame.ts)}</span>
                                <span class="frame-dir" title=direction_title>{direction_mark}</span>
                                <span class=kind_class>{row.frame.kind.clone()}</span>
                                <span class="frame-action">{clean(row.frame.action.as_deref().unwrap_or(""))}</span>
                                <span class="frame-summary">{summary(&row.frame)}</span>
                            </button>
                        }
                    }/>
                </div>
                <Show when=move || detail.get().is_some()>
                    <aside class="log-detail" aria-label="Frame detail">
                        <header class="section-head">
                            <h3>"Frame"</h3>
                            <button class="button" on:click=move |_| selected.set(None)>"Close"</button>
                        </header>
                        <pre class="json">{move || detail.get().unwrap_or_default()}</pre>
                    </aside>
                </Show>
            </div>
        </div>
    }
}
