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

use crate::app::{App, Effect, Msg, NoticeLevel, update};

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

/// Runs the UI until the user quits. Does not shut the station down; the caller does.
pub async fn run(handle: StationHandle, options: TuiOptions) -> Result<(), TuiError> {
    let mut guard = terminal::TerminalGuard::enter()?;
    let config = handle.config();
    let mut app = App::new(
        handle.snapshot(),
        text::redact_url(&config.url),
        config.vendor.clone(),
    );
    if let Ok(size) = guard.terminal().size() {
        app.size = (size.width, size.height);
    }

    let mut keys = EventStream::new();
    let mut station_events = handle.subscribe();
    let mut snapshots = handle.watch_snapshot();
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Msg>();
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    let runner = ScenarioRunner::new(handle.clone());
    let mut cancel = CancelToken::new();
    let mut tick = tokio::time::interval(options.tick_rate);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let signal = shutdown_signal();
    tokio::pin!(signal);

    while !app.quit {
        let msg = tokio::select! {
            ev = keys.next() => match ev {
                Some(Ok(TermEvent::Key(k))) => Msg::Key(k),
                Some(Ok(TermEvent::Paste(s))) => Msg::Paste(s),
                Some(Ok(TermEvent::Resize(w, h))) => Msg::Resize(w, h),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            ev = station_events.recv() => match ev {
                Ok(e) => Msg::Event(e),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    Msg::Notice(NoticeLevel::Error, format!("log overflow: {n} events dropped"))
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            changed = snapshots.changed() => {
                if changed.is_err() { break; }
                Msg::Snapshot(Box::new(snapshots.borrow_and_update().clone()))
            },
            Some(p) = progress_rx.recv() => Msg::Scenario(p),
            Some(m) = msg_rx.recv() => m,
            _ = tick.tick() => Msg::Tick,
            () = &mut signal => Msg::Signal,
        };
        let is_tick = matches!(msg, Msg::Tick);
        let effects = update(&mut app, msg);
        for effect in effects {
            execute(
                effect,
                &handle,
                &runner,
                &mut cancel,
                &msg_tx,
                &progress_tx,
                &mut guard,
            )
            .await;
        }
        if is_tick && app.dirty {
            guard.terminal().draw(|f| ui::draw(f, &app))?;
            app.dirty = false;
        }
    }
    Ok(())
}

async fn execute(
    effect: Effect,
    handle: &StationHandle,
    runner: &ScenarioRunner,
    cancel: &mut CancelToken,
    msg_tx: &mpsc::UnboundedSender<Msg>,
    progress_tx: &mpsc::UnboundedSender<kirby_ocpp_core::scenario::ScenarioProgress>,
    guard: &mut terminal::TerminalGuard,
) {
    match effect {
        Effect::Send {
            label,
            command,
            show_response,
        } => {
            let (h, tx) = (handle.clone(), msg_tx.clone());
            tokio::spawn(async move {
                let result = h.send(command).await.map_err(|e| e.to_string());
                let _ = tx.send(Msg::CommandDone {
                    label,
                    show_response,
                    result,
                });
            });
        }
        Effect::RunScenario(scenario) => {
            *cancel = CancelToken::new();
            spawn_scenario(*scenario, runner, cancel.clone(), msg_tx, progress_tx);
        }
        Effect::RunScenarioFile(path) => {
            match tokio::task::spawn_blocking(move || Scenario::load(std::path::Path::new(&path)))
                .await
            {
                Ok(Ok(scenario)) => {
                    *cancel = CancelToken::new();
                    spawn_scenario(scenario, runner, cancel.clone(), msg_tx, progress_tx);
                }
                Ok(Err(e)) => {
                    let _ = msg_tx.send(Msg::ScenarioError(e.to_string()));
                }
                Err(e) => {
                    let _ = msg_tx.send(Msg::ScenarioError(e.to_string()));
                }
            }
        }
        Effect::CancelScenario => cancel.cancel(),
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
            let _ = msg_tx.send(Msg::Notice(notice.0, notice.1));
        }
        Effect::WriteFile { path, contents } => {
            let notice = match write_private(&path, contents).await {
                Ok(()) => (NoticeLevel::Success, format!("log exported to {path}")),
                Err(e) => (NoticeLevel::Error, format!("export to {path} failed: {e}")),
            };
            let _ = msg_tx.send(Msg::Notice(notice.0, notice.1));
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

fn spawn_scenario(
    scenario: Scenario,
    runner: &ScenarioRunner,
    cancel: CancelToken,
    msg_tx: &mpsc::UnboundedSender<Msg>,
    progress_tx: &mpsc::UnboundedSender<kirby_ocpp_core::scenario::ScenarioProgress>,
) {
    let (runner, tx, progress) = (runner.clone(), msg_tx.clone(), progress_tx.clone());
    tokio::spawn(async move {
        let msg = match runner.run(&scenario, &cancel, Some(&progress)).await {
            Ok(report) => Msg::ScenarioDone(Box::new(report)),
            Err(e) => Msg::ScenarioError(e.to_string()),
        };
        let _ = tx.send(msg);
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
