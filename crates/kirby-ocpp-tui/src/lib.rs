//! Terminal UI for kirby-ocpp (ratatui + crossterm).
//!
//! The UI receives a [`StationHandle`] and talks to the station only through
//! `send(Command)`, `snapshot()` and `subscribe()`. The model ([`app`]) is pure:
//! [`app::update`] turns a [`app::Msg`] into [`app::Effect`]s, and [`run`] executes them.
//! The terminal is restored on every exit path (RAII guard + panic hook).

#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "catalogues and draw functions are long flat lists; ratatui code uses `f`, `w`, `h`"
)]

pub mod actions;
pub mod app;
pub mod browser;
pub mod clipboard;
pub mod fleet;
pub mod forms;
mod keys;
pub mod logview;
pub mod palette;
mod terminal;
#[cfg(test)]
mod tests_support;
pub mod text;
pub mod ui;

use std::io::Write;
use std::time::Duration;

use crossterm::event::{Event as TermEvent, EventStream};
use futures_util::StreamExt;
use kirby_ocpp_core::StationHandle;
use kirby_ocpp_core::scenario::{CancelToken, Scenario, ScenarioRunner};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};

use crate::app::{App, Effect, Msg, NoticeLevel};
use crate::fleet::{FleetApp, FleetEffect, FleetMsg, update_fleet};

/// UI tuning knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiOptions {
    /// Minimum time between redraws (drawing only happens when something changed).
    pub tick_rate: Duration,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            tick_rate: Duration::from_millis(33),
        }
    }
}

/// Why the UI ended abnormally.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TuiError {
    /// The terminal could not be set up or drawn to.
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Runs the UI for one station until the user quits. Does not shut the station down; the
/// caller does.
pub async fn run(handle: StationHandle, options: TuiOptions) -> Result<(), TuiError> {
    run_fleet(&[handle], options).await
}

/// What the loop receives from the station tasks.
enum Inbound {
    Msg(usize, Msg),
    /// The station's event channel closed (the station ended).
    Gone,
}

/// Sends messages for one station into the loop.
#[derive(Clone)]
struct Out {
    tx: mpsc::UnboundedSender<Inbound>,
    station: usize,
}

impl Out {
    fn send(&self, msg: Msg) {
        let _ = self.tx.send(Inbound::Msg(self.station, msg));
    }
}

/// What the loop needs per station besides the model.
struct Ctx {
    handle: StationHandle,
    runner: ScenarioRunner,
    cancel: CancelToken,
    out: Out,
}

/// Forwards one station's events and snapshots into the loop.
fn forward_station(handle: &StationHandle, out: &Out, tx: &mpsc::UnboundedSender<Inbound>) {
    let mut events = handle.subscribe();
    let mut snapshots = handle.watch_snapshot();
    let (out, tx) = (out.clone(), tx.clone());
    tokio::spawn(async move {
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Ok(e) => out.send(Msg::Event(e)),
                    Err(broadcast::error::RecvError::Lagged(n)) => out.send(Msg::Notice(
                        NoticeLevel::Error,
                        format!("log overflow: {n} events dropped"),
                    )),
                    Err(broadcast::error::RecvError::Closed) => {
                        let _ = tx.send(Inbound::Gone);
                        return;
                    }
                },
                changed = snapshots.changed() => {
                    if changed.is_err() {
                        let _ = tx.send(Inbound::Gone);
                        return;
                    }
                    out.send(Msg::Snapshot(Box::new(snapshots.borrow_and_update().clone())));
                }
            }
        }
    });
}

/// Runs the UI over one or more stations until the user quits. With more than one station
/// a selector row is shown (see [`fleet`]). Does not shut the stations down; the caller does.
pub async fn run_fleet(stations: &[StationHandle], options: TuiOptions) -> Result<(), TuiError> {
    let mut guard = terminal::TerminalGuard::enter()?;
    let apps: Vec<App> = stations
        .iter()
        .map(|handle| {
            let config = handle.config();
            App::new(
                handle.snapshot(),
                text::redact_url(&config.url),
                config.vendor.clone(),
            )
        })
        .collect();
    let mut model = FleetApp::new(apps);
    if let Ok(size) = guard.terminal().size() {
        for app in &mut model.apps {
            app.size = (size.width, size.height);
        }
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Inbound>();
    let mut ctxs: Vec<Ctx> = Vec::with_capacity(stations.len());
    for (station, handle) in stations.iter().enumerate() {
        let out = Out {
            tx: tx.clone(),
            station,
        };
        forward_station(handle, &out, &tx);
        ctxs.push(Ctx {
            handle: handle.clone(),
            runner: ScenarioRunner::new(handle.clone()),
            cancel: CancelToken::new(),
            out,
        });
    }
    let mut gone = 0;
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(options.tick_rate);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let signal = shutdown_signal();
    tokio::pin!(signal);

    while !model.quit {
        let msg = tokio::select! {
            ev = keys.next() => match ev {
                Some(Ok(TermEvent::Key(k))) => FleetMsg::Key(k),
                Some(Ok(TermEvent::Paste(s))) => FleetMsg::Paste(s),
                Some(Ok(TermEvent::Resize(w, h))) => FleetMsg::Resize(w, h),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            Some(inbound) = rx.recv() => match inbound {
                Inbound::Msg(station, msg) => FleetMsg::Station { station, msg },
                Inbound::Gone => {
                    gone += 1;
                    if gone >= ctxs.len() {
                        break;
                    }
                    continue;
                }
            },
            _ = tick.tick() => FleetMsg::Tick,
            () = &mut signal => FleetMsg::Signal,
        };
        let is_tick = matches!(msg, FleetMsg::Tick);
        for effect in update_fleet(&mut model, msg) {
            match effect {
                FleetEffect::Station { station, effect } => {
                    if let Some(ctx) = ctxs.get_mut(station) {
                        execute(effect, ctx, &mut guard).await;
                    }
                }
                FleetEffect::ConnectAll => {
                    for ctx in &ctxs {
                        let h = ctx.handle.clone();
                        let out = ctx.out.clone();
                        tokio::spawn(async move {
                            if let Err(e) = h.connect().await {
                                out.send(Msg::Notice(
                                    NoticeLevel::Error,
                                    format!("connect failed: {e}"),
                                ));
                            }
                        });
                    }
                }
                FleetEffect::DisconnectAll => {
                    for ctx in &ctxs {
                        let h = ctx.handle.clone();
                        let out = ctx.out.clone();
                        tokio::spawn(async move {
                            if let Err(e) = h.disconnect().await {
                                out.send(Msg::Notice(
                                    NoticeLevel::Error,
                                    format!("disconnect failed: {e}"),
                                ));
                            }
                        });
                    }
                }
            }
        }
        if is_tick && model.dirty {
            guard.terminal().draw(|f| ui::draw_fleet(f, &model))?;
            model.dirty = false;
            model.current_mut().dirty = false;
        }
    }
    for ctx in &ctxs {
        ctx.cancel.cancel();
    }
    Ok(())
}

async fn execute(effect: Effect, ctx: &mut Ctx, guard: &mut terminal::TerminalGuard) {
    match effect {
        Effect::Send {
            label,
            command,
            show_response,
        } => {
            let (h, out) = (ctx.handle.clone(), ctx.out.clone());
            tokio::spawn(async move {
                let result = h.send(command).await.map_err(|e| e.to_string());
                out.send(Msg::CommandDone {
                    label,
                    show_response,
                    result,
                });
            });
        }
        Effect::RunScenario(scenario) => {
            ctx.cancel = CancelToken::new();
            spawn_scenario(*scenario, ctx);
        }
        Effect::RunScenarioFile(path) => {
            match tokio::task::spawn_blocking(move || Scenario::load(std::path::Path::new(&path)))
                .await
            {
                Ok(Ok(scenario)) => {
                    ctx.cancel = CancelToken::new();
                    spawn_scenario(scenario, ctx);
                }
                Ok(Err(e)) => ctx.out.send(Msg::ScenarioError(e.to_string())),
                Err(e) => ctx.out.send(Msg::ScenarioError(e.to_string())),
            }
        }
        Effect::CancelScenario => ctx.cancel.cancel(),
        Effect::Copy { text, what } => {
            let ext = tokio::time::timeout(
                Duration::from_millis(500),
                tokio::task::spawn_blocking({
                    let text = text.clone();
                    move || clipboard::copy_external(&text)
                }),
            )
            .await;
            let notice = if let Ok(Ok(Some(tool))) = ext {
                (NoticeLevel::Success, format!("copied {what} ({tool})"))
            } else if let Some(seq) = clipboard::osc52(&text) {
                let out = guard.terminal().backend_mut();
                let _ = out.write_all(seq.as_bytes());
                let _ = out.flush();
                (
                    NoticeLevel::Success,
                    format!("copied {what} (OSC 52, needs terminal support)"),
                )
            } else {
                (
                    NoticeLevel::Error,
                    format!("{what} is too large to copy; use export"),
                )
            };
            ctx.out.send(Msg::Notice(notice.0, notice.1));
        }
        Effect::WriteFile { path, contents } => {
            let notice = match write_private(&path, contents).await {
                Ok(()) => (NoticeLevel::Success, format!("log exported to {path}")),
                Err(e) => (NoticeLevel::Error, format!("export to {path} failed: {e}")),
            };
            ctx.out.send(Msg::Notice(notice.0, notice.1));
        }
    }
}

/// Writes the export readable by the owner only: logs contain idTags and meter data.
async fn write_private(path: &str, contents: String) -> std::io::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(contents.as_bytes())
    })
    .await
    .map_err(std::io::Error::other)?
}

fn spawn_scenario(scenario: Scenario, ctx: &Ctx) {
    let (runner, out, cancel) = (ctx.runner.clone(), ctx.out.clone(), ctx.cancel.clone());
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    let relay = out.clone();
    tokio::spawn(async move {
        while let Some(p) = progress_rx.recv().await {
            relay.send(Msg::Scenario(p));
        }
    });
    tokio::spawn(async move {
        let msg = match runner.run(&scenario, &cancel, Some(&progress_tx)).await {
            Ok(report) => Msg::ScenarioDone(Box::new(report)),
            Err(e) => Msg::ScenarioError(e.to_string()),
        };
        drop(progress_tx);
        out.send(msg);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tick_rate_is_about_30_fps() {
        assert_eq!(TuiOptions::default().tick_rate, Duration::from_millis(33));
    }
}
