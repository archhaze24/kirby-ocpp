//! The event loop of the OCPP 2.x UI: keys, ticks, station events and effect execution.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{Event as TermEvent, EventStream};
use futures_util::StreamExt;
use kirby_ocpp_core::scenario::{CancelToken, Scenario, ScenarioProgress, run_scenario};
use kirby_ocpp_core::station2::{Outcome2, Station2Handle};
use tokio::sync::{broadcast, mpsc};

use super::app::{App2, Effect2, Msg2};
use super::draw2;
use super::update2;
use crate::app::NoticeLevel;
use crate::terminal::TerminalGuard;
use crate::{EVENT_QUEUE, Inbound, TuiError, TuiOptions, clipboard, text};

/// Sends messages for the station into the loop.
#[derive(Clone)]
struct Out {
    tx: mpsc::UnboundedSender<Inbound<Msg2>>,
}

impl Out {
    fn send(&self, msg: Msg2) {
        let _ = self.tx.send(Inbound::Msg(0, msg));
    }
}

/// What the loop needs besides the model.
struct Ctx {
    handle: Station2Handle,
    out: Out,
    /// `true` while a scenario runs; at most one runs at a time.
    scenario_running: Arc<AtomicBool>,
    cancel: CancelToken,
}

/// Forwards the station's events and snapshots into the loop through a bounded queue, so a
/// stalled draw cannot make memory grow.
fn forward_station(handle: &Station2Handle, tx: &mpsc::Sender<Inbound<Msg2>>) {
    let mut events = handle.subscribe();
    let mut snapshots = handle.watch_snapshot();
    let (handle, tx) = (handle.clone(), tx.clone());
    tokio::spawn(async move {
        let mut dropped = 0u64;
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Ok(e) => crate::deliver(&tx, 0, Msg2::Event(e), &mut dropped),
                    Err(broadcast::error::RecvError::Lagged(n)) => dropped += n,
                    Err(broadcast::error::RecvError::Closed) => {
                        let _ = tx.send(Inbound::Gone).await;
                        return;
                    }
                },
                changed = snapshots.changed() => {
                    if changed.is_err() {
                        let _ = tx.send(Inbound::Gone).await;
                        return;
                    }
                    let snapshot = Msg2::Snapshot(Box::new(snapshots.borrow_and_update().clone()));
                    crate::deliver(&tx, 0, snapshot, &mut dropped);
                }
                // Retries the resync while nothing else happens.
                () = tokio::time::sleep(Duration::from_millis(100)), if dropped > 0 => {}
            }
            crate::resync_with(
                &tx,
                0,
                &mut dropped,
                || Msg2::Snapshot(Box::new(handle.snapshot())),
                |text| Msg2::Notice(NoticeLevel::Error, text),
            );
        }
    });
}

/// Runs the UI for one OCPP 2.x station until the user quits. Does not shut the station
/// down; the caller does.
///
/// # Errors
///
/// Fails when the terminal cannot be set up or drawn to.
pub async fn run2(handle: Station2Handle, options: TuiOptions) -> Result<(), TuiError> {
    let mut guard = TerminalGuard::enter()?;
    let config = handle.config();
    let mut app = App2::new(
        handle.snapshot(),
        text::redact_url(&config.url),
        config.vendor.clone(),
    );
    app.mono = options.monochrome;
    app.time = options.time;
    if let Ok(size) = guard.terminal().size() {
        app.size = (size.width, size.height);
    }

    // User-action results (few, never dropped) and forwarded station events (bounded).
    let (user_tx, mut user_rx) = mpsc::unbounded_channel::<Inbound<Msg2>>();
    let (event_tx, mut event_rx) = mpsc::channel::<Inbound<Msg2>>(EVENT_QUEUE);
    forward_station(&handle, &event_tx);
    let mut ctx = Ctx {
        handle,
        out: Out { tx: user_tx },
        scenario_running: Arc::new(AtomicBool::new(false)),
        cancel: CancelToken::new(),
    };
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(options.tick_rate);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let signal = crate::shutdown_signal();
    tokio::pin!(signal);

    while !app.quit {
        let msg = tokio::select! {
            ev = keys.next() => match ev {
                Some(Ok(TermEvent::Key(k))) => Msg2::Key(k),
                Some(Ok(TermEvent::Paste(s))) => Msg2::Paste(s),
                Some(Ok(TermEvent::Resize(w, h))) => Msg2::Resize(w, h),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            Some(inbound) = async {
                tokio::select! {
                    m = user_rx.recv() => m,
                    m = event_rx.recv() => m,
                }
            } => match inbound {
                Inbound::Msg(_, msg) => msg,
                Inbound::Gone => break,
            },
            _ = tick.tick() => Msg2::Tick,
            () = &mut signal => Msg2::Signal,
        };
        let is_tick = matches!(msg, Msg2::Tick);
        for effect in update2(&mut app, msg) {
            execute(effect, &mut ctx, &mut guard).await;
        }
        if is_tick && app.dirty {
            guard.terminal().draw(|f| draw2(f, &app))?;
            app.dirty = false;
        }
    }
    ctx.cancel.cancel();
    Ok(())
}

async fn execute(effect: Effect2, ctx: &mut Ctx, guard: &mut TerminalGuard) {
    match effect {
        Effect2::Send {
            label,
            commands,
            show_response,
        } => {
            let (handle, out) = (ctx.handle.clone(), ctx.out.clone());
            tokio::spawn(async move {
                let mut result = Ok(Outcome2::Done);
                for command in commands {
                    match handle.send(command).await {
                        Ok(outcome) => result = Ok(outcome),
                        Err(e) => {
                            result = Err(e.to_string());
                            break;
                        }
                    }
                }
                out.send(Msg2::CommandDone {
                    label,
                    show_response,
                    result,
                });
            });
        }
        Effect2::RunScenario(scenario) => {
            if ctx.scenario_running.load(Ordering::SeqCst) {
                ctx.out.send(Msg2::ScenarioError(
                    "a scenario is already running".to_owned(),
                ));
                return;
            }
            ctx.cancel = CancelToken::new();
            spawn_scenario(*scenario, ctx);
        }
        Effect2::RunScenarioFile(path) => {
            if ctx.scenario_running.load(Ordering::SeqCst) {
                ctx.out.send(Msg2::ScenarioError(
                    "a scenario is already running".to_owned(),
                ));
                return;
            }
            ctx.cancel = CancelToken::new();
            let parts = Ctx {
                handle: ctx.handle.clone(),
                out: ctx.out.clone(),
                scenario_running: Arc::clone(&ctx.scenario_running),
                cancel: ctx.cancel.clone(),
            };
            tokio::spawn(async move {
                let out = parts.out.clone();
                match tokio::task::spawn_blocking(move || {
                    Scenario::load(std::path::Path::new(&path))
                })
                .await
                {
                    Ok(Ok(scenario)) => spawn_scenario(scenario, &parts),
                    Ok(Err(e)) => out.send(Msg2::ScenarioError(e.to_string())),
                    Err(e) => out.send(Msg2::ScenarioError(e.to_string())),
                }
            });
        }
        Effect2::CancelScenario => ctx.cancel.cancel(),
        Effect2::Copy { text, what } => {
            let (level, text) =
                clipboard::copy_with_fallback(&text, &what, guard.terminal().backend_mut()).await;
            ctx.out.send(Msg2::Notice(level, text));
        }
        Effect2::WriteFile { path, contents } => {
            let notice = match crate::write_private(&path, contents).await {
                Ok(()) => (NoticeLevel::Success, format!("log exported to {path}")),
                Err(e) => (NoticeLevel::Error, format!("export to {path} failed: {e}")),
            };
            ctx.out.send(Msg2::Notice(notice.0, notice.1));
        }
    }
}

/// Runs `scenario` in the background; the report (or the refusal, when another scenario got
/// there first) goes back into the loop. Progress is relayed while it runs.
fn spawn_scenario(scenario: Scenario, ctx: &Ctx) {
    if ctx.scenario_running.swap(true, Ordering::SeqCst) {
        ctx.out.send(Msg2::ScenarioError(
            "a scenario is already running".to_owned(),
        ));
        return;
    }
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ScenarioProgress>();
    let relay = ctx.out.clone();
    tokio::spawn(async move {
        while let Some(p) = progress_rx.recv().await {
            relay.send(Msg2::Scenario(Box::new(p)));
        }
    });
    let (handle, out, cancel, running) = (
        ctx.handle.clone(),
        ctx.out.clone(),
        ctx.cancel.clone(),
        Arc::clone(&ctx.scenario_running),
    );
    tokio::spawn(async move {
        let report = run_scenario(&handle, &scenario, &cancel, Some(&progress_tx)).await;
        running.store(false, Ordering::SeqCst);
        drop(progress_tx);
        out.send(Msg2::ScenarioDone(Box::new(report)));
    });
}
