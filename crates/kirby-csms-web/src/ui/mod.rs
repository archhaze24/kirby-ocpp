//! Components and helpers shared by the screens. Everything that comes from a station is shown as
//! text.

use leptos::prelude::*;

/// `ocpp2.1` as `2.1`; other values unchanged, `none` for a station without a subprotocol.
pub fn protocol_label(protocol: Option<&str>) -> String {
    protocol.map_or_else(
        || "none".to_owned(),
        |p| p.strip_prefix("ocpp").unwrap_or(p).to_owned(),
    )
}

/// The CSS class of a connector status: the part of the status name that decides the color.
pub fn status_class(status: &str) -> &'static str {
    match status {
        "Available" => "ok",
        "Preparing" | "Charging" | "Occupied" | "SuspendedEV" | "SuspendedEVSE" | "Finishing" => {
            "busy"
        }
        "Reserved" => "info",
        "Faulted" => "bad",
        "Unavailable" => "off",
        _ => "idle",
    }
}

/// A round marker in the color of a state.
#[component]
pub fn Dot(
    #[prop(into)] class: Signal<&'static str>,
    #[prop(into, optional)] title: String,
) -> impl IntoView {
    view! { <span class=move || format!("dot dot-{}", class.get()) title=title.clone() aria-hidden="true"></span> }
}

/// A small label such as the protocol version.
#[component]
pub fn Badge(
    #[prop(into)] text: Signal<String>,
    #[prop(into, optional)] tone: String,
) -> impl IntoView {
    view! { <span class=format!("badge {tone}")>{move || text.get()}</span> }
}

/// An empty-state line.
#[component]
pub fn Empty(children: Children) -> impl IntoView {
    view! { <p class="empty">{children()}</p> }
}

/// An error message that stays in view until the cause is fixed.
#[component]
pub fn ErrorLine(#[prop(into)] text: Signal<Option<String>>) -> impl IntoView {
    view! {
        <Show when=move || text.get().is_some()>
            <p class="error" role="alert">{move || text.get().unwrap_or_default()}</p>
        </Show>
    }
}

/// A section with a title and optional actions at the right of the title.
#[component]
pub fn Section(
    #[prop(into)] title: String,
    #[prop(optional)] actions: Option<ChildrenFn>,
    children: Children,
) -> impl IntoView {
    view! {
        <section class="section">
            <header class="section-head">
                <h3>{title}</h3>
                <div class="section-actions">{actions.map(|actions| actions())}</div>
            </header>
            {children()}
        </section>
    }
}

/// Whole watt-hours as `12.34 kWh` and watts as `7.4 kW` (or the plain unit below 1000).
pub fn energy(wh: f64) -> String {
    if wh.abs() >= 1000.0 {
        format!("{:.2} kWh", wh / 1000.0)
    } else {
        format!("{wh:.0} Wh")
    }
}

pub fn power(w: f64) -> String {
    if w.abs() >= 1000.0 {
        format!("{:.2} kW", w / 1000.0)
    } else {
        format!("{w:.0} W")
    }
}

/// Delivered energy of a transaction: the latest register reading minus the start reading.
#[allow(clippy::cast_precision_loss)]
pub fn delivered_wh(tx: &crate::api::Transaction) -> Option<f64> {
    let latest = tx.meter_stop.map(|stop| stop as f64).or(tx.energy_wh)?;
    Some(latest - tx.meter_start.unwrap_or(0) as f64)
}

/// `1.23 EUR` style amount.
pub fn money(amount: f64, currency: &str) -> String {
    format!("{amount:.2} {currency}")
}
