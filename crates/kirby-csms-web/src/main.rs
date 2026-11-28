//! Web UI of `kirby-csms`: a client-side rendered Leptos app that talks to the `/v1` HTTP API of
//! the listener that served it.

mod api;
mod app;
mod commands;
mod log;
mod rules;
mod stations;
mod store;
mod transactions;
mod ui;
mod util;

fn main() {
    leptos::mount::mount_to_body(app::App);
}
