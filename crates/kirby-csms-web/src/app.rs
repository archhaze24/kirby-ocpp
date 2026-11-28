//! The page frame: header, station sidebar, the main area and the token prompt.

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::stations::{Sidebar, StationPage};
use crate::store::{Link, Store, View, use_store};
use crate::transactions::TransactionsView;
use crate::util;

const THEME_KEY: &str = "kirby-csms-theme";

fn apply_theme(theme: Option<&str>) {
    if let Some(root) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
    {
        let _ = match theme {
            Some(theme) => root.set_attribute("data-theme", theme),
            None => root.remove_attribute("data-theme"),
        };
    }
}

fn system_prefers_dark() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
        .is_some_and(|query| query.matches())
}

#[component]
pub fn App() -> impl IntoView {
    let store = Store::new();
    provide_context(store);
    store.start();

    // `/` focuses the station filter, `Escape` closes the drawer. Typing in a field is left alone.
    window_event_listener(ev::keydown, move |event| {
        let in_field = event
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
            .is_some_and(|el| matches!(el.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT"));
        if event.key() == "/" && !in_field && !event.ctrl_key() && !event.meta_key() {
            if let Some(input) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id("station-filter"))
                .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
            {
                event.prevent_default();
                store.view.set(View::Stations);
                store.drawer.set(true);
                // The list may be hidden until this change is drawn.
                request_animation_frame(move || {
                    let _ = input.focus();
                });
            }
        } else if event.key() == "Escape" {
            store.drawer.set(false);
        }
    });

    view! {
        <Header/>
        <div
            class="layout"
            class:drawer-open=move || store.drawer.get()
            class:wide=move || store.view.get() == View::Transactions
        >
            <Sidebar/>
            <button
                class="scrim"
                aria-label="Close the station list"
                tabindex="-1"
                on:click=move |_| store.drawer.set(false)
            ></button>
            <main class="main">
                {move || match store.view.get() {
                    View::Transactions => view! { <TransactionsView/> }.into_any(),
                    View::Stations => match store.selected.get() {
                        Some(id) => view! { <StationPage id=id/> }.into_any(),
                        None => view! { <Welcome/> }.into_any(),
                    },
                }}
            </main>
        </div>
        <TokenPrompt/>
    }
}

#[component]
fn Welcome() -> impl IntoView {
    let store = use_store();
    view! {
        <div class="welcome">
            <h2>"Select a station"</h2>
            <p class="muted">
                {move || {
                    if store.stations.with(Vec::is_empty) {
                        "No station has connected yet. Point a charge point or the emulator at this server and it shows up here."
                    } else {
                        "Pick a station in the list to see its state, frames and commands."
                    }
                }}
            </p>
            // Narrow screens hide the list in a drawer: open it from here too.
            {move || {
                (!store.stations.with(Vec::is_empty)).then(|| view! {
                    <button
                        class="button primary welcome-list"
                        on:click=move |_| store.drawer.set(true)
                    >"Show stations"</button>
                })
            }}
        </div>
    }
}

#[component]
fn Header() -> impl IntoView {
    let store = use_store();
    let dark = RwSignal::new(false);
    // The effective theme at start: the stored choice, else the system setting.
    let stored = util::stored(false, THEME_KEY).filter(|t| t == "light" || t == "dark");
    dark.set(
        stored
            .as_deref()
            .map_or_else(system_prefers_dark, |t| t == "dark"),
    );
    apply_theme(stored.as_deref());
    let toggle = move |_| {
        let next = !dark.get_untracked();
        dark.set(next);
        let name = if next { "dark" } else { "light" };
        util::store(false, THEME_KEY, Some(name));
        apply_theme(Some(name));
    };
    let connected = move || {
        store
            .stations
            .with(|stations| stations.iter().filter(|s| s.connected).count())
    };
    let live = move || match store.live.get() {
        Link::Open => ("live", "Live"),
        Link::Connecting => ("connecting", "Connecting"),
        Link::Reconnecting => ("reconnecting", "Reconnecting"),
    };
    view! {
        <header class="topbar">
            <button
                class="icon-button menu-button"
                aria-label="Station list"
                on:click=move |_| store.drawer.update(|open| *open = !*open)
            >"☰"</button>
            <h1 class="brand">"kirby-csms"</h1>
            <span class="version">{move || {
                let version = store.version.get();
                if version.is_empty() { String::new() } else { format!("v{version}") }
            }}</span>
            <nav class="views" aria-label="Views">
                <button
                    class="view-button"
                    class:active=move || store.view.get() == View::Stations
                    on:click=move |_| store.view.set(View::Stations)
                >"Stations"</button>
                <button
                    class="view-button"
                    class:active=move || store.view.get() == View::Transactions
                    on:click=move |_| store.view.set(View::Transactions)
                >"Transactions"</button>
            </nav>
            <span class="spacer"></span>
            <span class="count">
                {move || {
                    let total = store.stations.with(Vec::len);
                    let noun = if total == 1 { "station" } else { "stations" };
                    format!("{total} {noun}, {} connected", connected())
                }}
            </span>
            <span class=move || format!("live live-{}", live().0) role="status">
                <span class="live-dot" aria-hidden="true"></span>
                {move || live().1}
            </span>
            <button class="icon-button" aria-label="Switch light and dark theme" on:click=toggle>
                {move || if dark.get() { "☀" } else { "☾" }}
            </button>
        </header>
    }
}

#[component]
fn TokenPrompt() -> impl IntoView {
    let store = use_store();
    let value = RwSignal::new(String::new());
    let submit = move || {
        let token = value.get_untracked();
        let token = token.trim();
        if !token.is_empty() {
            store.set_token(Some(token.to_owned()));
            value.set(String::new());
        }
    };
    view! {
        <Show when=move || store.need_token.get()>
            <div class="modal-backdrop">
                <form
                    class="modal"
                    role="dialog"
                    aria-modal="true"
                    aria-labelledby="token-title"
                    on:submit=move |event| {
                        event.prevent_default();
                        submit();
                    }
                >
                    <h2 id="token-title">"API token"</h2>
                    <p class="muted">
                        {move || if store.token.get().is_some() {
                            "The server refused this token. Enter the right one."
                        } else {
                            "This server needs an API token (--api-token). It is kept for this browser tab only."
                        }}
                    </p>
                    <input
                        class="input"
                        type="password"
                        autocomplete="off"
                        autofocus
                        aria-label="API token"
                        prop:value=move || value.get()
                        on:input=move |event| value.set(event_target_value(&event))
                    />
                    <div class="modal-actions">
                        <button class="button primary" type="submit">"Continue"</button>
                    </div>
                </form>
            </div>
        </Show>
    }
}
